pub mod router;
pub mod ui;

use derive_more::Display;
use fpsdk::{
    create_plugin,
    plugin::{message::DebugLogMsg, Plugin, PluginProxy},
    ProcessParamFlags,
};
use parking_lot::Mutex;
use router::Router;
use serde::{Deserialize, Serialize};
use shared_memory::Shmem;
use std::{collections::VecDeque, fmt::Debug, io::Read, panic::RefUnwindSafe};
use uuid::Uuid;

type Sample = [f32; 2];

#[derive(Debug, PartialEq, Display, Clone, Copy, Eq, Serialize, Deserialize)]
pub enum Mode {
    Receiver,
    Sender,
}

impl Mode {
    const ALL: [Mode; 2] = [Mode::Receiver, Mode::Sender];
}

#[derive(Deserialize, Serialize, Debug, Clone)]
pub enum SaveState {
    Ver1 { mode: Mode, uuid: uuid::Uuid },
}

#[derive(Debug, Clone)]
pub enum PluginStateChange {
    AvailableChannels(Vec<Uuid>),
    ChannelId(Uuid),
    Mode(Mode),
}

struct Feedback {
    host: Mutex<fpsdk::host::Host>,
    tag: fpsdk::plugin::Tag,
    handle: Option<fpsdk::plugin::PluginProxy>,
    mode: Mode,
    memory: Shmem,
    store: Mutex<VecDeque<Sample>>,
    uuid: Option<uuid::Uuid>,

    ui_handle: ui::UIHandle,
}

impl std::fmt::Debug for Feedback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Feedback")
            .field("host", &self.host)
            .field("tag", &self.tag)
            .field("handle", &self.handle)
            .field("mode", &self.mode)
            .field("memory", &"Shmem { ... }")
            .finish()
    }
}

unsafe impl Send for Feedback {}
unsafe impl Sync for Feedback {}

impl Feedback {
    fn router(&self) -> &mut Router {
        unsafe {
            let ptr: *mut *mut Router = std::mem::transmute(self.memory.as_ptr());
            (*ptr).as_mut().unwrap()
        }
    }

    fn log(&self, msg: String) {
        self.host.lock().on_message(self.tag, DebugLogMsg(msg));
    }

    fn set_channel(&mut self, uuid: Uuid) {
        // Clear our store
        // Dump our channel
        // Set our id
        self.store.lock().clear();
        self.router()
            .rx(&uuid)
            .map(|c| while let Ok(_) = c.try_recv() {});

        self.uuid = Some(uuid);

        // Inform UI of this
        self.send_channel_id();
    }

    fn send_available_channels(&self) {
        self.ui_handle
            .send_sync(ui::UIMessage::StateChange(
                PluginStateChange::AvailableChannels(self.router().ids()),
            ))
            .unwrap();
    }

    fn send_channel_id(&self) {
        self.ui_handle
            .send_sync(ui::UIMessage::StateChange(PluginStateChange::ChannelId(
                self.uuid.clone().or(Some(Uuid::nil())).unwrap(),
            )))
            .unwrap();
    }

    fn send_mode(&self) {
        self.ui_handle
            .send_sync(ui::UIMessage::StateChange(PluginStateChange::Mode(
                self.mode,
            )))
            .unwrap();
    }
}

// TODO(emily): This is what we call a _lie_
impl RefUnwindSafe for Feedback {}

impl Plugin for Feedback {
    fn new(host: fpsdk::host::Host, tag: fpsdk::plugin::Tag) -> Self
    where
        Self: Sized,
    {
        let config = shared_memory::ShmemConf::new()
            .size(std::mem::size_of::<*mut *mut Router>())
            .os_id(format!("emilydotgg-feedback-{}", std::process::id()));
        let open_config = config.clone();
        let memory = if let Ok(mut memory) = config.create() {
            // TODO(emily): This probably needs to not be a box and be some reference counting structure
            // so that this doesn't blow up immediately
            let channels = Box::leak(Box::new(Router::new()));

            unsafe {
                let ptr: *mut *mut Router = std::mem::transmute(memory.as_ptr());
                *ptr = channels;
            }

            memory.set_owner(true);

            memory
        } else {
            open_config.open().unwrap()
        };

        Self {
            host: Mutex::new(host),
            tag,
            handle: None,
            mode: Mode::Receiver,
            memory,
            store: Default::default(),
            uuid: None,
            ui_handle: ui::UIHandle::new(),
        }
    }

    fn info(&self) -> fpsdk::plugin::Info {
        fpsdk::plugin::InfoBuilder::new_effect("emilydotgg-feedback", "feedback", 1)
            .want_new_tick()
            .build()
    }

    fn save_state(&mut self, writer: fpsdk::plugin::StateWriter) {
        if let Some(uuid) = self.uuid {
            let state = SaveState::Ver1 {
                mode: self.mode,
                uuid: uuid,
            };

            bincode::serialize_into(writer, &state).unwrap();
        }
    }

    fn load_state(&mut self, mut reader: fpsdk::plugin::StateReader) {
        let mut buf: Vec<u8> = vec![];
        reader
            .read_to_end(&mut buf)
            .and_then(|_| {
                bincode::deserialize::<SaveState>(&buf).map_err(|e| {
                    std::io::Error::new(
                        std::io::ErrorKind::Other,
                        format!("error deserializing value {}", e),
                    )
                })
            })
            .map(|value| match value {
                SaveState::Ver1 { mode, uuid } => {
                    self.mode = mode;
                    if let None = self.router().channel(&uuid) {
                        self.router().new_channel_with_id(&uuid);
                    }
                    self.set_channel(uuid);
                    self.send_mode();
                }
            })
            .unwrap_or_else(|_e| self.log(format!("error reading state")));
        // No load state
    }

    fn on_message(&mut self, message: fpsdk::host::Message<'_>) -> Box<dyn fpsdk::AsRawPtr> {
        match message {
            fpsdk::host::Message::ShowEditor(hwnd) => {
                self.send_available_channels();
                self.ui_handle
                    .send_sync(ui::UIMessage::ShowEditor(hwnd))
                    .unwrap();
            }
            _ => {}
        }

        // TODO(emily): This really needs to happen somewhere else
        while let Ok(msg) = self.ui_handle.rx.try_recv() {
            match msg {
                ui::PluginMessage::SetEditor(hwnd) => {
                    if let Some(handle) = self.handle.as_ref() {
                        handle.set_editor_hwnd(hwnd.unwrap_or(0 as *mut c_void));
                    }
                }
                ui::PluginMessage::NewChannel => {
                    let id = self.router().new_channel();
                    self.set_channel(id);
                }
                ui::PluginMessage::SelectChannel(id) => self.set_channel(id),
                ui::PluginMessage::SetMode(mode) => {
                    println!("self.mode = {mode}");
                    self.mode = mode
                }
                ui::PluginMessage::AskChannels => self.send_available_channels(),
            }
        }
        Box::new(0)
    }

    fn name_of(&self, _value: fpsdk::host::GetName) -> String {
        "No names".into()
    }

    fn process_event(&mut self, _event: fpsdk::host::Event) {}

    fn process_param(
        &mut self,
        _index: usize,
        value: fpsdk::ValuePtr,
        flags: fpsdk::ProcessParamFlags,
    ) -> Box<dyn fpsdk::AsRawPtr> {
        self.log(format!("process_param"));

        if flags.contains(ProcessParamFlags::FROM_MIDI | ProcessParamFlags::UPDATE_VALUE) {
            // Scale speed into a more appropriate range
            // it will be 0 - 65535 coming in and we want it to be less

            let value = value.get::<u32>();
            self.log(format!("value is {value}"));

            if value > 65535 {
                self.mode = Mode::Sender
            } else {
                self.mode = Mode::Receiver
            }

            self.log(format!("mode is {:?}", self.mode));
        }

        Box::new(0)
    }

    fn idle(&mut self) {}

    fn tick(&mut self) {}

    fn render(&mut self, input: &[[f32; 2]], output: &mut [[f32; 2]]) {
        const HIGH_MARK: usize = 4096;
        const LOW_MARK: usize = 256;

        match self.mode {
            Mode::Receiver => {
                let mut store = self.store.lock();
                // Try and receive more samples
                if let Some(rx) = self.uuid.as_ref().and_then(|uuid| self.router().rx(uuid)) {
                    if store.len() < LOW_MARK {
                        while store.len() < HIGH_MARK {
                            match rx.try_recv() {
                                Ok(samples) => {
                                    for s in samples {
                                        store.push_back(s)
                                    }
                                }
                                Err(_err) => {
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    self.log(format!("no rx?"));
                }
                if store.len() < output.len() {
                    self.log(format!("underrun: {} vs {}", store.len(), output.len()));
                    return;
                } else {
                    for os in output.iter_mut() {
                        *os = store.pop_front().unwrap();
                    }
                }
            }
            Mode::Sender => {
                if let Some(tx) = self.uuid.as_ref().and_then(|uuid| self.router().tx(uuid)) {
                    tx.send(Vec::from(input)).unwrap();
                }
            }
        }
    }

    fn voice_handler(&mut self) -> Option<&mut dyn fpsdk::voice::ReceiveVoiceHandler> {
        None
    }

    fn proxy(&mut self, handle: PluginProxy) {
        self.handle = Some(handle)
    }
}

impl Drop for Feedback {
    fn drop(&mut self) {
        self.ui_handle.send_sync(ui::UIMessage::Die).unwrap();
        self.ui_handle.join();
    }
}

create_plugin!(Feedback);
