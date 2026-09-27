use libc::timespec;
use objc2::{AnyThread, DefinedClass, Encode, Encoding, define_class, msg_send, rc::Retained};
use objc2_foundation::{NSObject, NSObjectProtocol};
use objc2_fs_kit::{FSItem, FSItemAttributes, FSItemID, FSItemType};

#[derive(Debug)]
pub(crate) struct ItemIvars {
    pub attributes: Retained<FSItemAttributes>,
}

define_class!(
    #[derive(Debug)]
    #[unsafe(super(FSItem, NSObject))]
    #[ivars = ItemIvars]
    pub(crate) struct Item;

    unsafe impl NSObjectProtocol for Item {}
);

impl Item {
    pub(crate) fn attributes(&self) -> &Retained<FSItemAttributes> {
        &self.ivars().attributes
    }

    pub(crate) fn new(entry: &crate::Metadata, store_timestamps: bool) -> Retained<Self> {
        let id = FSItemID(entry.id);
        let attributes = unsafe { FSItemAttributes::new() };

        unsafe { attributes.setFileID(id) };

        unsafe {
            attributes.setParentID(FSItemID(entry.parent));
            attributes.setUid(libc::getuid());
            attributes.setGid(libc::getgid());
            attributes.setLinkCount(1);
            attributes.setType(match entry.kind {
                crate::FileKind::Directory => FSItemType::Directory,
                crate::FileKind::File => FSItemType::File,
                crate::FileKind::Symlink => FSItemType::Symlink,
            });
            attributes.setMode(u32::from(entry.mode));
            attributes.setSize(entry.size);
            attributes.setAllocSize(entry.size);
            attributes.setFlags(0);
        }

        let timespec = timespec {
            tv_sec: if store_timestamps { 1 } else { 0 },
            tv_nsec: 0,
        };

        #[repr(transparent)]
        #[derive(Clone, Copy)]
        struct Timespec(timespec);

        unsafe impl Encode for Timespec {
            const ENCODING: Encoding =
                Encoding::Struct("timespec", &[Encoding::C_LONG, Encoding::C_LONG]);
        }

        let timespec = Timespec(timespec);
        let _: () = unsafe { msg_send![&*attributes, setAddedTime: timespec] };
        let _: () = unsafe { msg_send![&*attributes, setBirthTime: timespec] };
        let _: () = unsafe { msg_send![&*attributes, setChangeTime: timespec] };
        let _: () = unsafe { msg_send![&*attributes, setModifyTime: timespec] };
        let _: () = unsafe { msg_send![&*attributes, setAccessTime: timespec] };

        let this = Self::alloc().set_ivars(ItemIvars {
            attributes,
            // Set other item state.
        });
        unsafe { msg_send![super(this), init] }
    }
}
