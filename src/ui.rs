use std::{sync::Arc, thread::JoinHandle};

use eyre::Result;

use futures::stream;
use iced::{Alignment, Application, Padding};
use parking_lot::Mutex;
use tokio::sync::mpsc;
use uuid::Uuid;

use crate::{Mode, PluginStateChange};

pub mod window_handle;

/// Message sent from Plugin to UI
#[derive(Debug, Clone)]
pub enum UIMessage {
    ShowEditor(window_handle::WindowHandle),
    StateChange(PluginStateChange),
    Die,
}

/// Message sent from UI to Plugin
#[derive(Debug)]
pub enum PluginMessage {
    SetEditor(window_handle::WindowHandle),
    SetMode(Mode),
    NewChannel,
    SelectChannel(Uuid),
    AskChannels,
}

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
            settings.window.visible = false;
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
    hwnd: Mutex<window_handle::WindowHandle>,
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
                hwnd: Mutex::new(window_handle::WindowHandle::null()),
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
                let change_mode_command =
                    iced::window::change_mode::<Message>(if handle.is_valid() {
                        iced::window::Mode::Windowed
                    } else {
                        iced::window::Mode::Hidden
                    });

                unsafe {
                    use windows::Win32::UI::WindowsAndMessaging;
                    let self_hwnd = self.hwnd.lock().as_hwnd();

                    let show_cmd = if handle.is_valid() {
                        WindowsAndMessaging::SW_SHOW
                    } else {
                        WindowsAndMessaging::SW_HIDE
                    };

                    WindowsAndMessaging::SetParent(self_hwnd, handle.as_hwnd());
                    WindowsAndMessaging::ShowWindow(self_hwnd, WindowsAndMessaging::SW_SHOW);
                }

                // NOTE(emily): Send our (iced's) hwnd to FL to set as the editor window
                let message = PluginMessage::SetEditor(self.hwnd.lock().clone());
                let host_message_tx = self.tx.clone();

                Some(iced::Command::batch([
                    iced::Command::perform(
                        async move {
                            host_message_tx.send(message).await.unwrap();
                        },
                        |_| Message::None,
                    ),
                    change_mode_command,
                ]))
            }
            Message::PluginMessage(UIMessage::StateChange(state_change)) => {
                match state_change {
                    PluginStateChange::AvailableChannels(channels) => {
                        self.available_channels = channels;
                    }
                    PluginStateChange::ChannelId(id) => {
                        self.selected_channel = Some(id);
                    }
                    PluginStateChange::Mode(mode) => {
                        self.selected_mode = Some(mode);
                    }
                };
                None
            }
            Message::PluginMessage(UIMessage::Die) => Some(iced::window::close::<Message>()),

            Message::ModeSelected(new_mode) => {
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
        *self.hwnd.lock() = hwnd.into();
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
