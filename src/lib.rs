pub mod ui;

use derive_more::{Deref, DerefMut, Display};
use fpsdk::{
    create_plugin,
    plugin::{message::DebugLogMsg, Plugin, PluginProxy},
    ProcessParamFlags,
};
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use shared_memory::Shmem;
use std::{
    collections::{HashMap, VecDeque},
    fmt::Debug,
    panic::RefUnwindSafe,
    sync::{mpsc, Arc},
};
use uuid::Uuid;

#[derive(Debug, PartialEq, Display, Clone, Copy, Eq)]
pub enum Mode {
    Receiver,
    Sender,
}

impl Mode {
    const ALL: [Mode; 2] = [Mode::Receiver, Mode::Sender];
}

type Sample = [f32; 2];

struct Channels<T>(mpsc::Sender<T>, mpsc::Receiver<T>);

impl<T> Channels<T> {
    fn new() -> Self {
        let (tx, rx) = mpsc::channel::<T>();

        Self(tx, rx)
    }
}

struct _Router {
    channels: HashMap<Uuid, Channels<Vec<Sample>>>,
}

impl _Router {
    fn new() -> Self {
        Self {
            channels: Default::default(),
        }
    }

    fn new_channel(&mut self) -> Uuid {
        let new_uuid = Uuid::new_v4();
        self.channels.insert(new_uuid, Channels::new());
        new_uuid
    }

    fn channel(&mut self, uuid: &Uuid) -> Option<&mut Channels<Vec<Sample>>> {
        self.channels.get_mut(uuid)
    }
}

#[derive(Deref, DerefMut)]
struct Router(Mutex<_Router>);

impl Router {
    fn new() -> Self {
        Self(Mutex::new(_Router::new()))
    }

    fn new_channel(&self) -> Uuid {
        self.lock().new_channel()
    }

    fn channel(&self, uuid: &Uuid) -> Option<MappedMutexGuard<Channels<Vec<Sample>>>> {
        MutexGuard::try_map(self.lock(), |s| s.channel(uuid)).ok()
    }

    fn rx(&self, uuid: &Uuid) -> Option<MappedMutexGuard<mpsc::Receiver<Vec<Sample>>>> {
        self.channel(uuid)
            .map(|c| MappedMutexGuard::map(c, |o| &mut o.1))
    }

    fn tx(&self, uuid: &Uuid) -> Option<MappedMutexGuard<mpsc::Sender<Vec<Sample>>>> {
        self.channel(uuid)
            .map(|c| MappedMutexGuard::map(c, |o| &mut o.0))
    }

    fn ids(&self) -> Vec<Uuid> {
        self.lock().channels.keys().map(|k| *k).collect()
    }
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
            .channel(&uuid)
            .map(|c| while let Ok(_) = c.1.try_recv() {});

        self.uuid = Some(uuid);
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
            .size(100)
            .os_id(format!("emilydotgg-feedback-{}", std::process::id()));
        let open_config = config.clone();
        let mut memory = if let Ok(mut memory) = config.create() {
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
        // No stave state
    }

    fn load_state(&mut self, reader: fpsdk::plugin::StateReader) {
        // No load state
    }

    fn on_message(&mut self, message: fpsdk::host::Message<'_>) -> Box<dyn fpsdk::AsRawPtr> {
        match message {
            fpsdk::host::Message::ShowEditor(hwnd) => {
                self.ui_handle
                    .send_sync(ui::UIMessage::AvailableChannels(self.router().ids()))
                    .unwrap();
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
                    self.ui_handle
                        .send_sync(ui::UIMessage::NewChannelId(id))
                        .unwrap();
                }
                ui::PluginMessage::SelectChannel(id) => self.set_channel(id),
                ui::PluginMessage::SetMode(mode) => {
                    println!("self.mode = {mode}");
                    self.mode = mode
                }
                ui::PluginMessage::AskChannels => self
                    .ui_handle
                    .send_sync(ui::UIMessage::AvailableChannels(self.router().ids()))
                    .unwrap(),
            }
        }
        Box::new(0)
    }

    fn name_of(&self, value: fpsdk::host::GetName) -> String {
        "No names".into()
    }

    fn process_event(&mut self, _event: fpsdk::host::Event) {}

    fn process_param(
        &mut self,
        index: usize,
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
                                Err(err) => {
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

    fn midi_in(&mut self, _message: fpsdk::MidiMessage) {}

    fn loop_in(&mut self, _message: fpsdk::ValuePtr) {}

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
