use std::{collections::HashMap, sync::mpsc};

use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use uuid::Uuid;

use crate::Sample;

pub struct Channels<T>(mpsc::SyncSender<T>, mpsc::Receiver<T>);

impl<T> Channels<T> {
    fn new() -> Self {
        let (tx, rx) = mpsc::sync_channel::<T>(10);

        Self(tx, rx)
    }
}

type SamplesChannels = Channels<Vec<Sample>>;

struct _Router {
    channels: HashMap<Uuid, SamplesChannels>,
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

    fn new_channel_with_id(&mut self, uuid: &Uuid) {
        self.channels.insert(uuid.clone(), Channels::new());
    }

    fn channel(&mut self, uuid: &Uuid) -> Option<&mut SamplesChannels> {
        self.channels.get_mut(uuid)
    }
}

pub struct Router(Mutex<_Router>);

impl Router {
    pub fn new() -> Self {
        Self(Mutex::new(_Router::new()))
    }

    pub fn new_channel(&self) -> Uuid {
        self.0.lock().new_channel()
    }

    pub fn new_channel_with_id(&self, uuid: &Uuid) {
        self.0.lock().new_channel_with_id(uuid)
    }

    pub fn channel(&self, uuid: &Uuid) -> Option<MappedMutexGuard<SamplesChannels>> {
        MutexGuard::try_map(self.0.lock(), |s| s.channel(uuid)).ok()
    }

    pub fn rx(&self, uuid: &Uuid) -> Option<MappedMutexGuard<mpsc::Receiver<Vec<Sample>>>> {
        self.channel(uuid)
            .map(|c| MappedMutexGuard::map(c, |o| &mut o.1))
    }

    pub fn tx(&self, uuid: &Uuid) -> Option<MappedMutexGuard<mpsc::SyncSender<Vec<Sample>>>> {
        self.channel(uuid)
            .map(|c| MappedMutexGuard::map(c, |o| &mut o.0))
    }

    pub fn ids(&self) -> Vec<Uuid> {
        self.0.lock().channels.keys().map(|k| *k).collect()
    }
}
