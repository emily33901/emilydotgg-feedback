pub mod ui;

use fpsdk::{
    create_plugin,
    plugin::{message::DebugLogMsg, Plugin, PluginProxy},
    ProcessParamFlags,
};
use shared_memory::Shmem;
use std::{
    collections::VecDeque,
    fmt::Debug,
    panic::RefUnwindSafe,
    sync::{mpsc, Arc},
};

#[derive(Debug, PartialEq)]
enum Mode {
    Receiver,
    Sender,
}

type Sample = [f32; 2];

struct Channels {
    tx: mpsc::Sender<Vec<Sample>>,
    rx: mpsc::Receiver<Vec<Sample>>,
}

struct Feedback {
    host: fpsdk::host::Host,
    tag: fpsdk::plugin::Tag,
    handle: Option<fpsdk::plugin::PluginProxy>,
    mode: Mode,
    last_mode: Mode,
    memory: Shmem,
    store: VecDeque<Sample>,
    uuid: uuid::Uuid,

    ui_handle: ui::UIHandle,
}

impl std::fmt::Debug for Feedback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Feedback")
            .field("host", &self.host)
            .field("tag", &self.tag)
            .field("handle", &self.handle)
            .field("mode", &self.mode)
            .field("last_mode", &self.last_mode)
            .field("memory", &"Shmem { ... }")
            .finish()
    }
}

unsafe impl Send for Feedback {}
unsafe impl Sync for Feedback {}

impl Feedback {
    fn channels(&self) -> &mut Channels {
        unsafe {
            let ptr: *mut *mut Channels = std::mem::transmute(self.memory.as_ptr());
            (*ptr).as_mut().unwrap()
        }
    }

    fn log(&mut self, msg: String) {
        self.host.on_message(self.tag, DebugLogMsg(msg));
    }
}

// TODO(emily): This is what we call a _lie_
impl RefUnwindSafe for Feedback {}

impl Drop for Feedback {
    fn drop(&mut self) {}
}

impl Plugin for Feedback {
    fn new(host: fpsdk::host::Host, tag: fpsdk::plugin::Tag) -> Self
    where
        Self: Sized,
    {
        unsafe { windows::Win32::System::Console::AllocConsole() };

        let config = shared_memory::ShmemConf::new()
            .size(100)
            .os_id(format!("emilydotgg-feedback-{}", std::process::id()));
        let open_config = config.clone();
        let mut memory = if let Ok(mut memory) = config.create() {
            let (tx, rx) = mpsc::channel::<Vec<Sample>>();
            // TODO(emily): This probably needs to not be a box and be some reference counting structure
            // so that this doesn't blow up immediately
            let channels = Box::leak(Box::new(Channels { tx, rx }));

            unsafe {
                let ptr: *mut *mut Channels = std::mem::transmute(memory.as_ptr());
                *ptr = channels;
            }

            memory.set_owner(true);

            memory
        } else {
            open_config.open().unwrap()
        };

        Self {
            host,
            tag,
            handle: None,
            mode: Mode::Receiver,
            last_mode: Mode::Receiver,
            memory,
            store: Default::default(),
            uuid: uuid::Uuid::new_v4(),
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
        // No message handling 🤠
        match message {
            fpsdk::host::Message::ShowEditor(hwnd) => self
                .ui_handle
                .send_sync(ui::UIMessage::ShowEditor(hwnd))
                .unwrap(),
            default => {}
        }

        while let Ok(msg) = self.ui_handle.rx.try_recv() {
            match msg {
                ui::PluginMessage::SetEditor(hwnd) => {
                    if let Some(handle) = self.handle.as_ref() {
                        handle.set_editor_hwnd(hwnd.unwrap_or(0 as *mut c_void));
                    }
                }
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
        self.host
            .on_message(self.tag, DebugLogMsg(format!("process_param")));

        if flags.contains(ProcessParamFlags::FROM_MIDI | ProcessParamFlags::UPDATE_VALUE) {
            // Scale speed into a more appropriate range
            // it will be 0 - 65535 coming in and we want it to be less

            let value = value.get::<u32>();
            self.host
                .on_message(self.tag, DebugLogMsg(format!("value is {value}")));

            if value > 65535 {
                self.mode = Mode::Sender
            } else {
                self.mode = Mode::Receiver
            }

            self.host
                .on_message(self.tag, DebugLogMsg(format!("mode is {:?}", self.mode)));
        }

        Box::new(0)
    }

    fn idle(&mut self) {}

    fn tick(&mut self) {}

    fn render(&mut self, input: &[[f32; 2]], output: &mut [[f32; 2]]) {
        const HIGH_MARK: usize = 4096;
        const LOW_MARK: usize = 1024;

        match self.mode {
            Mode::Receiver => {
                // Try and receive more samples
                if self.store.len() < LOW_MARK {
                    while self.store.len() < HIGH_MARK {
                        let maybe_samples = { self.channels().rx.try_recv() };
                        if let Ok(samples) = maybe_samples {
                            for s in samples {
                                self.store.push_back(s)
                            }
                        } else {
                            break;
                        }
                    }
                }
                if self.store.len() < output.len() {
                    self.log(format!(
                        "underrun: {} vs {}",
                        self.store.len(),
                        output.len()
                    ));
                    return;
                } else {
                    for os in output.iter_mut() {
                        *os = self.store.pop_front().unwrap();
                    }
                }
            }
            Mode::Sender => {
                self.channels().tx.send(Vec::from(input)).unwrap();
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

create_plugin!(Feedback);
