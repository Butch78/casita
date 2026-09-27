use super::{
    Resource, configuration,
    util::{io_err, posix_err},
    volume::Volume,
};
use objc2::{
    AnyThread, DefinedClass, Message, define_class, msg_send,
    rc::{Allocated, Retained},
};
use objc2_foundation::{NSError, NSObjectProtocol, NSProgress, NSString, NSURL, NSUUID};
use objc2_fs_kit::{
    FSBlockDeviceResource, FSContainerIdentifier, FSContainerStatus, FSFileSystemBase,
    FSManageableResourceMaintenanceOperations, FSPathURLResource, FSProbeResult, FSResource,
    FSTask, FSTaskOptions, FSUnaryFileSystem, FSUnaryFileSystemOperations, FSVolume,
};
use std::{
    ffi::{CStr, OsStr},
    io,
    os::unix::ffi::OsStrExt,
    path::PathBuf,
    ptr::null_mut,
    sync::Mutex,
};

#[derive(PartialEq, Eq)]
enum Key {
    Path(PathBuf),
    Block(String),
}

fn key(resource: &FSResource) -> io::Result<Key> {
    if let Some(path) = resource.downcast_ref::<FSPathURLResource>() {
        if !configuration().path_resources {
            return Err(io::Error::from_raw_os_error(libc::EINVAL));
        }
        let url = unsafe { path.url() };
        let bytes = url.fileSystemRepresentation();
        // NSURL owns the NUL-terminated representation; copy before releasing it.
        return Ok(Key::Path(PathBuf::from(OsStr::from_bytes(
            unsafe { CStr::from_ptr(bytes.as_ptr()) }.to_bytes(),
        ))));
    }
    if let Some(block) = resource.downcast_ref::<FSBlockDeviceResource>()
        && configuration().block_resources
    {
        return Ok(Key::Block(unsafe { block.BSDName() }.to_string()));
    }
    Err(io::Error::from_raw_os_error(libc::EINVAL))
}

struct Scope(Retained<NSURL>);
impl Drop for Scope {
    fn drop(&mut self) {
        unsafe {
            self.0.stopAccessingSecurityScopedResource();
        }
    }
}

struct Loaded {
    key: Key,
    volume: Retained<Volume>,
    // Keep permission alive until the volume/backend has been dropped.
    _scope: Option<Scope>,
}

pub struct State {
    loaded: Mutex<Option<Loaded>>,
}

define_class!(
    #[unsafe(super(FSUnaryFileSystem))]
    #[name = "FSKitNativeFileSystem"]
    #[ivars = State]
    pub(crate) struct FileSystem;

    impl FileSystem {
        #[unsafe(method_id(init))]
        fn init(this: Allocated<Self>) -> Retained<Self> {
            let this = this.set_ivars(State { loaded: Mutex::new(None) });
            unsafe { msg_send![super(this), init] }
        }
    }

    unsafe impl NSObjectProtocol for FileSystem {}
    unsafe impl FSUnaryFileSystemOperations for FileSystem {
        #[unsafe(method(probeResource:replyHandler:))]
        fn probe(&self, resource: &FSResource, reply: &block2::DynBlock<dyn Fn(*mut FSProbeResult, *mut NSError)>) {
            if let Err(error) = key(resource) { reply.call((null_mut(), io_err(&error))); return; }
            let id = unsafe { FSContainerIdentifier::initWithUUID(FSContainerIdentifier::alloc(), &NSUUID::UUID()) };
            let name = NSString::from_str(configuration().name);
            let probe = unsafe { FSProbeResult::usableProbeResultWithName_containerID(&name, &id) };
            reply.call((Retained::as_ptr(&probe).cast_mut(), null_mut()));
        }

        #[unsafe(method(loadResource:options:replyHandler:))]
        fn load(&self, resource: &FSResource, _options: &FSTaskOptions, reply: &block2::DynBlock<dyn Fn(*mut FSVolume, *mut NSError)>) {
            let result = self.load_backend(resource);
            match result {
                Ok(volume) => {
                    unsafe { self.setContainerStatus(&FSContainerStatus::ready()); }
                    reply.call((Retained::as_ptr(&volume).cast_mut().cast(), null_mut()));
                }
                Err(error) => { eprintln!("filesystem load: {error}"); reply.call((null_mut(), io_err(&error))); }
            }
        }

        #[unsafe(method(unloadResource:options:replyHandler:))]
        fn unload(&self, resource: &FSResource, _options: &FSTaskOptions, reply: &block2::DynBlock<dyn Fn(*mut NSError)>) {
            let result = (|| {
                let resource_key = key(resource)?;
                let mut loaded = self.ivars().loaded.lock().map_err(|_| io::Error::other("resource lock poisoned"))?;
                if let Some(previous) = loaded.as_ref() {
                    if previous.key != resource_key { return Err(io::Error::from_raw_os_error(libc::EINVAL)); }
                    previous.volume.close()?;
                }
                drop(loaded.take());
                Ok::<_, io::Error>(())
            })();
            reply.call((result.err().as_ref().map_or(null_mut(), io_err),));
        }

        #[unsafe(method(didFinishLoading))]
        fn loaded(&self) {}
    }

    unsafe impl FSManageableResourceMaintenanceOperations for FileSystem {
        #[unsafe(method(startCheckWithTask:options:error:))]
        fn check(&self, _task: &FSTask, _options: &FSTaskOptions, _error: *mut *mut NSError) -> *mut NSProgress {
            let progress = NSProgress::progressWithTotalUnitCount(100);
            progress.setCompletedUnitCount(100);
            Retained::autorelease_ptr(progress)
        }

        #[unsafe(method(startFormatWithTask:options:error:))]
        fn format(&self, _task: &FSTask, _options: &FSTaskOptions, error: *mut *mut NSError) -> *mut NSProgress {
            if !error.is_null() { unsafe { *error = posix_err(libc::EROFS); } }
            null_mut()
        }
    }
);

impl FileSystem {
    fn load_backend(&self, resource: &FSResource) -> io::Result<Retained<Volume>> {
        // Serialize load/unload, including security-scope acquisition and failure.
        let mut loaded = self
            .ivars()
            .loaded
            .lock()
            .map_err(|_| io::Error::other("resource lock poisoned"))?;
        if loaded.is_some() {
            return Err(io::Error::from_raw_os_error(libc::EBUSY));
        }
        let key = key(resource)?;
        let scope = if let Some(path) = resource.downcast_ref::<FSPathURLResource>() {
            let url = unsafe { path.url() };
            if !unsafe { url.startAccessingSecurityScopedResource() } {
                return Err(io::Error::from_raw_os_error(libc::EACCES));
            }
            Some(Scope(url))
        } else {
            None
        };
        let resource = match &key {
            Key::Path(path) => Resource::Path(path),
            Key::Block(name) => Resource::BlockDevice(name),
        };
        let backend = (configuration().open)(resource)?;
        let volume = Volume::new(backend);
        *loaded = Some(Loaded {
            key,
            volume: volume.retain(),
            _scope: scope,
        });
        Ok(volume)
    }
}
