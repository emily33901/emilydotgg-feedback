pub mod router;
pub mod ui;

use derive_more::Display;
use fpsdk::{
    create_plugin,
    plugin::{message::DebugLogMsg, Plugin, PluginProxy},
};
use parking_lot::Mutex;
use router::SharedRouter;
use serde::{Deserialize, Serialize};
use std::{
    collections::VecDeque, fmt::Debug, io::Read, panic::RefUnwindSafe, sync::mpsc::TrySendError,
};
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
    store: Mutex<VecDeque<Sample>>,
    uuid: Option<uuid::Uuid>,
    router: SharedRouter,

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
    fn log(&self, msg: String) {
        self.host.lock().on_message(self.tag, DebugLogMsg(msg));
    }

    fn set_channel(&mut self, uuid: Uuid) {
        // Clear our store
        // Dump our channel
        // Set our id
        self.store.lock().clear();
        self.router
            .rx(&uuid)
            .map(|c| while let Ok(_) = c.try_recv() {});

        self.uuid = Some(uuid);

        // Inform UI of this
        self.send_channel_id();
    }

    fn send_available_channels(&self) {
        self.ui_handle
            .send_sync(ui::UIMessage::StateChange(
                PluginStateChange::AvailableChannels(self.router.ids()),
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

    fn receive_samples(&mut self) {
        const HIGH_MARK: usize = 4096;
        const LOW_MARK: usize = 256;

        let mut store = self.store.lock();

        // If we already have enough samples, early out
        if store.len() > LOW_MARK {
            return;
        }

        // Try and receive more samples
        if let Some(rx) = self.uuid.as_ref().and_then(|uuid| self.router.rx(uuid)) {
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
        } else {
            self.log(format!("no rx?"));
        }
    }
}

// TODO(emily): This is what we call a _lie_
impl RefUnwindSafe for Feedback {}

impl Plugin for Feedback {
    fn new(host: fpsdk::host::Host, tag: fpsdk::plugin::Tag) -> Self
    where
        Self: Sized,
    {
        unsafe {
            windows::Win32::System::Console::AllocConsole();
        }

        let router =
            SharedRouter::new_or_open(&format!("emilydotgg-feedback-{}", std::process::id()));

        Self {
            host: Mutex::new(host),
            tag,
            handle: None,
            mode: Mode::Receiver,
            store: Default::default(),
            uuid: None,
            ui_handle: ui::UIHandle::new(),
            router,
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
                    {
                        if let None = self.router.channel(&uuid) {
                            self.router.new_channel_with_id(&uuid);
                        }
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
                    let id = self.router.new_channel();
                    self.set_channel(id);
                }
                ui::PluginMessage::SelectChannel(id) => self.set_channel(id),
                ui::PluginMessage::SetMode(mode) => self.mode = mode,
                ui::PluginMessage::AskChannels => self.send_available_channels(),
            }
        }
        Box::new(0)
    }

    fn name_of(&self, _value: fpsdk::host::GetName) -> String {
        "No names".into()
    }

    fn process_event(&mut self, _event: fpsdk::host::Event) {}

    fn idle(&mut self) {}

    fn tick(&mut self) {}

    fn render(&mut self, input: &[[f32; 2]], output: &mut [[f32; 2]]) {
        match self.mode {
            Mode::Receiver => {
                self.receive_samples();

                let mut store = self.store.lock();
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
                if let Some(tx) = self.uuid.as_ref().and_then(|uuid| self.router.tx(uuid)) {
                    // NOTE(emily): We try send here, as we don't care whether this channel is full or not
                    // if it is full, there is probably no receiver anyway, so we just dump data here.

                    match tx.try_send(Vec::from(input)) {
                        Err(TrySendError::Full(_)) | Ok(_) => Ok(()),
                        e => e,
                    }
                    .unwrap()
                }
            }
        }
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
