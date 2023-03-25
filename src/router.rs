use std::{
    collections::HashMap,
    sync::{mpsc, Arc, Weak},
};

use derive_more::Deref;
use parking_lot::{MappedMutexGuard, Mutex, MutexGuard};
use shared_memory::Shmem;
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

    // TODO(emily): tx can (and should) return a clone of the sender, so as to not hold on to the mutex forever
    pub fn tx(&self, uuid: &Uuid) -> Option<MappedMutexGuard<mpsc::SyncSender<Vec<Sample>>>> {
        self.channel(uuid)
            .map(|c| MappedMutexGuard::map(c, |o| &mut o.0))
    }

    pub fn ids(&self) -> Vec<Uuid> {
        self.0.lock().channels.keys().map(|k| *k).collect()
    }
}

pub struct _SharedRouter(Option<(Router, Shmem)>);

impl std::ops::Deref for _SharedRouter {
    type Target = Router;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().map(|s| &s.0).unwrap()
    }
}
impl _SharedRouter {}

impl Drop for _SharedRouter {
    fn drop(&mut self) {
        // We were dropped (last person holding the shared memory)
        // Now we need to clear ourselves up
        if let Some((_router, mut shmem)) = self.0.take() {
            shmem.set_owner(true);
            let zelf_box =
                unsafe { Weak::from_raw(*(shmem.as_ptr() as *mut *const _SharedRouter)) };
            drop(zelf_box);
            drop(shmem)
        }
    }
}

#[derive(Deref, Clone)]
pub struct SharedRouter(Arc<_SharedRouter>);

impl SharedRouter {
    pub fn new_or_open(name: &str) -> SharedRouter {
        let config = shared_memory::ShmemConf::new()
            .size(std::mem::size_of::<*mut *const _SharedRouter>())
            .os_id(name);
        let open_config = config.clone();
        if let Ok(memory) = config.create() {
            let mem_ptr = memory.as_ptr();

            let inner = Arc::new(_SharedRouter(Some((Router::new(), memory))));
            let weak = Arc::downgrade(&inner);

            unsafe {
                *std::mem::transmute::<*mut u8, *mut *const _SharedRouter>(mem_ptr) =
                    weak.into_raw();
            }

            SharedRouter(inner)
        } else {
            let memory = open_config.open().unwrap();

            unsafe {
                // Get Pointer to _SharedRouter from shared memory
                let ptr = *(memory.as_ptr() as *mut *const _SharedRouter);

                Arc::increment_strong_count(ptr);
                let inner = Arc::from_raw(ptr);

                SharedRouter(inner)
            }
        }
    }
}
