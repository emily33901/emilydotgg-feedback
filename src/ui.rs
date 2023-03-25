use std::{ffi::c_void, sync::Arc, thread::JoinHandle};

use eyre::Result;

use futures::stream;
use iced::window;
use iced::{Alignment, Application, Padding};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::Mode;

/// Message sent from Plugin to UI
#[derive(Debug, Clone)]
pub enum UIMessage {
    ShowEditor(Option<*mut c_void>),
    NewChannelId(Uuid),
    AvailableChannels(Vec<Uuid>),
    Die,
}

/// Message sent from UI to Plugin
#[derive(Debug)]
pub enum PluginMessage {
    SetEditor(Option<*mut c_void>),
    SetMode(Mode),
    NewChannel,
    SelectChannel(Uuid),
    AskChannels,
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
            settings.window.size = (200, 200);
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

    pub fn join(&self) {
        self.thread_handle.lock().take().map(|h| {
            h.join().unwrap();
        });
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
    selected_channel: Option<Uuid>,
    selected_mode: Option<Mode>,
    available_channels: Vec<Uuid>,
}

#[derive(Debug, Clone)]
enum Message {
    /// A message from the Plugin to the UI
    PluginMessage(UIMessage),
    ModeSelected(Mode),
    ChannelSelected(Uuid),
    NewChannel,
    None,
}

impl iced::Application for UI {
    type Message = Message;

    type Flags = UIFlags;
    type Executor = iced::executor::Default;

    fn new(flags: Self::Flags) -> (Self, iced::Command<Self::Message>) {
        (
            Self {
                tx: flags.tx.clone(),
                rx: Arc::new(tokio::sync::Mutex::new(flags.rx)),
                should_draw: false,
                hwnd: Mutex::new(None),
                selected_channel: None,
                selected_mode: Some(Mode::Receiver),
                available_channels: vec![],
            },
            iced::Command::batch([iced::Command::perform(
                async move {
                    flags.tx.send(PluginMessage::AskChannels).await.unwrap();
                },
                |_| Message::None,
            )]),
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
            Message::PluginMessage(UIMessage::NewChannelId(id)) => {
                println!("New Channel {id}");

                self.selected_channel = Some(id);

                None
            }
            Message::PluginMessage(UIMessage::AvailableChannels(channels)) => {
                self.available_channels = channels;

                None
            }
            Message::PluginMessage(UIMessage::Die) => Some(iced::window::close::<Message>()),

            Message::ModeSelected(new_mode) => {
                println!("Mode {new_mode}");
                self.selected_mode = Some(new_mode);

                let host_message_tx = self.tx.clone();
                Some(iced::Command::perform(
                    async move {
                        host_message_tx
                            .send(PluginMessage::SetMode(new_mode))
                            .await
                            .unwrap()
                    },
                    |_| Message::None,
                ))
            }
            Message::ChannelSelected(channel) => {
                self.selected_channel = Some(channel);

                let host_message_tx = self.tx.clone();
                Some(iced::Command::perform(
                    async move {
                        host_message_tx
                            .send(PluginMessage::SelectChannel(channel))
                            .await
                            .unwrap()
                    },
                    |_| Message::None,
                ))
            }
            Message::NewChannel => {
                println!("New channel");

                let host_message_tx = self.tx.clone();
                Some(iced::Command::perform(
                    async move {
                        host_message_tx
                            .send(PluginMessage::NewChannel)
                            .await
                            .unwrap()
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
        iced::widget::column!(
            iced::widget::text("emilydotgg-feedback"),
            iced::widget::pick_list(&Mode::ALL[..], self.selected_mode, Message::ModeSelected),
            iced::widget::pick_list(
                &self.available_channels,
                self.selected_channel,
                Message::ChannelSelected
            ),
            iced::widget::button("New channel").on_press(Message::NewChannel),
        )
        .align_items(Alignment::Center)
        .padding(Padding::new(10.0))
        .spacing(20)
        .into()
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
        Box::pin(futures::stream::unfold(self, |state| async move {
            state.rx.lock().await.recv().await.map_or(None, |message| {
                Some((Message::PluginMessage(message), state.clone()))
            })
        }))
    }
}
