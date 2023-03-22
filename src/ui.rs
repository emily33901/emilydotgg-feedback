use std::{ffi::c_void, sync::Arc, thread::JoinHandle};

use eyre::Result;

use futures::stream;
use iced::Application;
use parking_lot::Mutex;
use tokio::sync::mpsc;

/// Message sent from Plugin to UI
#[derive(Debug)]
pub enum UIMessage {
    ShowEditor(Option<*mut c_void>),
}

/// Message sent from UI to Plugin
#[derive(Debug)]
pub enum PluginMessage {
    SetEditor(Option<*mut c_void>),
}

unsafe impl Send for PluginMessage {}
unsafe impl Sync for PluginMessage {}

unsafe impl Send for UIMessage {}
unsafe impl Sync for UIMessage {}

pub struct UIHandle {
    thread_handle: Mutex<Option<JoinHandle<()>>>,
    tx: mpsc::Sender<UIMessage>,
    pub rx: mpsc::Receiver<PluginMessage>,
}

impl UIHandle {
    pub fn new() -> Self {
        let (ui_tx, ui_rx) = mpsc::channel::<UIMessage>(10);
        let (plugin_tx, plugin_rx) = mpsc::channel::<PluginMessage>(10);

        let zelf = Self {
            thread_handle: Mutex::new(None),
            tx: ui_tx,
            rx: plugin_rx,
        };

        let ui_thread = std::thread::spawn(|| {
            let mut settings = iced::Settings::with_flags(UIFlags {
                rx: ui_rx,
                tx: plugin_tx,
            });
            settings.antialiasing = true;
            settings.window.resizable = false;
            settings.window.decorations = false;
            UI::run(settings).unwrap();
        });

        *zelf.thread_handle.lock() = Some(ui_thread);
        zelf
    }

    pub fn send_sync(&self, message: UIMessage) -> Result<()> {
        self.tx.blocking_send(message)?;
        Ok(())
    }
}

struct UIFlags {
    rx: mpsc::Receiver<UIMessage>,
    tx: mpsc::Sender<PluginMessage>,
}

struct UI {
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<UIMessage>>>,
    tx: mpsc::Sender<PluginMessage>,
    should_draw: bool,
    hwnd: Mutex<Option<*mut c_void>>,
}

#[derive(Debug)]
enum Message {
    /// A message from the Plugin to the UI
    PluginMessage(UIMessage),
    None,
}

impl iced::Application for UI {
    type Message = Message;

    type Flags = UIFlags;
    type Executor = iced::executor::Default;

    fn new(flags: Self::Flags) -> (Self, iced::Command<Self::Message>) {
        (
            Self {
                tx: flags.tx,
                rx: Arc::new(tokio::sync::Mutex::new(flags.rx)),
                should_draw: false,
                hwnd: Mutex::new(None),
            },
            iced::Command::none(),
        )
    }

    fn title(&self) -> String {
        "emilydotgg-feedback".into()
    }

    fn theme(&self) -> Self::Theme {
        iced::Theme::Dark
    }

    fn update(&mut self, message: Self::Message) -> iced::Command<Self::Message> {
        match message {
            Message::PluginMessage(UIMessage::ShowEditor(handle)) => {
                self.should_draw = handle.is_some();
                unsafe {
                    use windows::Win32::Foundation::*;
                    use windows::Win32::UI::WindowsAndMessaging;
                    let self_hwnd = self.hwnd.lock();
                    let self_hwnd = HWND(self_hwnd.map_or(0, |x| x as isize) as isize);
                    if let Some(parent_hwnd) = handle.map(|x| HWND(x as isize)) {
                        WindowsAndMessaging::SetParent(self_hwnd, parent_hwnd);
                        WindowsAndMessaging::ShowWindow(self_hwnd, WindowsAndMessaging::SW_SHOW);
                    } else {
                        WindowsAndMessaging::ShowWindow(self_hwnd, WindowsAndMessaging::SW_HIDE);
                        WindowsAndMessaging::SetParent(self_hwnd, HWND(0));
                    }
                }

                let message = PluginMessage::SetEditor(self.hwnd.lock().clone());
                let host_message_tx = self.tx.clone();

                Some(iced::Command::perform(
                    async move {
                        host_message_tx.send(message).await.unwrap();
                    },
                    |_| Message::None,
                ))
            }
            _ => None,
        }
        .or(Some(iced::Command::none()))
        .unwrap()
    }

    fn subscription(&self) -> iced::Subscription<Self::Message> {
        iced_native::subscription::Subscription::batch([iced_native::Subscription::from_recipe(
            UIMessageWatcher {
                rx: self.rx.clone(),
            },
        )])
    }

    fn view(&self) -> iced::Element<'_, Self::Message, iced::Renderer<Self::Theme>> {
        iced::widget::column!(iced::widget::text("emilydotgg-feedback")).into()
    }

    fn hwnd(&self, hwnd: *mut std::ffi::c_void) {
        *self.hwnd.lock() = Some(hwnd);
    }

    type Theme = iced::Theme;
}

#[derive(Clone)]
struct UIMessageWatcher {
    rx: Arc<tokio::sync::Mutex<mpsc::Receiver<UIMessage>>>,
}

impl<H, Event> iced_native::subscription::Recipe<H, Event> for UIMessageWatcher
where
    H: std::hash::Hasher,
{
    type Output = Message;

    fn hash(&self, state: &mut H) {
        use std::hash::Hash;

        std::any::TypeId::of::<Self>().hash(state);
        0.hash(state);
    }

    fn stream(
        self: Box<Self>,
        _input: stream::BoxStream<Event>,
    ) -> stream::BoxStream<Self::Output> {
        Box::pin(futures::stream::unfold(self, |mut state| async move {
            state.rx.lock().await.recv().await.map_or(None, |message| {
                Some((Message::PluginMessage(message), state.clone()))
            })
        }))
    }
}
