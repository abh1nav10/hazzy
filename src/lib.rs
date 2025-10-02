use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicPtr};

// let new = Box::into_raw(Box::new(Ret {
//ptr : AtomicPtr::new(std::ptr::null_mut()),
//  next : AtomicPtr::new(std::ptr::null_mut()),
//}));

static SHARED_DOMAIN: HazPtrDomain = HazPtrDomain {
    list: HazPtrs {
        head: AtomicPtr::new(std::ptr::null_mut()),
    },
    ret: Retired {
        head: AtomicPtr::new(std::ptr::null_mut()),
    },
};

pub struct HazPtrHolder(Option<&'static HazPtr>);

impl HazPtrHolder {
    pub fn load<'a, T>(&'a mut self, ptr: &'_ AtomicPtr<T>) -> Option<&'a T> {
        let hazptr = if let Some(t) = self.0 {
            t
        } else {
            let ptr = SHARED_DOMAIN.acquire();
            self.0 = Some(ptr);
            ptr
        };
        let mut ptr1 = ptr.load(Ordering::SeqCst);
        hazptr.protect(ptr1 as *mut ());
        let ret = loop {
            let ptr2 = ptr.load(Ordering::SeqCst);
            if ptr1 == ptr2 {
                if let Some(_) = NonNull::new(ptr1) {
                    let ret = unsafe { ptr1.as_ref() };
                    break ret;
                } else {
                    break None;
                }
            } else {
                ptr1 = ptr2;
            }
        };
        self.reset();
        return ret;
    }

    pub fn store<T>(&mut self, value: T) {
        todo!()
    }

    pub fn reset(&mut self) {
        if let Some(t) = self.0 {
            t.ptr.store(std::ptr::null_mut(), Ordering::SeqCst);
            t.flag.store(true, Ordering::SeqCst);
            self.0 = None;
        }
    }
}

struct HazPtr {
    ptr: AtomicPtr<()>,
    next: AtomicPtr<HazPtr>,
    flag: AtomicBool,
}

impl HazPtr {
    pub fn protect(&self, ptr: *mut ()) {
        self.ptr.store(ptr, Ordering::SeqCst);
    }
}

pub trait HazPtrObject {
    fn domain(&self) -> &HazPtrDomain;
    fn retire(ptr: *mut ());
}

pub struct HazPtrObjectWrapper<'a: 'b, 'b, T> {
    inner: &'b T,
    domain: &'a HazPtrDomain,
}

impl<T> HazPtrObject for HazPtrObjectWrapper<'_, '_, T> {
    fn domain(&self) -> &HazPtrDomain {
        self.domain
    }
    fn retire(ptr: *mut ()) {
        todo!()
    }
}

impl<T> Deref for HazPtrObjectWrapper<'_, '_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        self.inner
    }
}

pub struct HazPtrDomain {
    list: HazPtrs,
    ret: Retired,
}

impl HazPtrDomain {
    pub fn acquire(&self) -> &'static HazPtr {
        if self.list.head.load(Ordering::SeqCst).is_null() {
            let mut hazptr = HazPtr {
                ptr: AtomicPtr::new(std::ptr::null_mut()),
                next: AtomicPtr::new(std::ptr::null_mut()),
                flag: AtomicBool::new(false),
            };
            let raw = Box::leak(Box::new(hazptr));
            if self
                .list
                .head
                .compare_exchange(
                    std::ptr::null_mut(),
                    raw,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                return unsafe { &*raw };
            } else {
                let drop = unsafe { Box::from_raw(raw) };
                std::mem::drop(drop);
            }
        }
        let mut current = (&self.list.head).load(Ordering::SeqCst);
        while !current.is_null() {
            if unsafe { &(*current).flag }
                .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                return unsafe { &(*current) };
            } else {
                current = unsafe { (&(*current).next).load(Ordering::SeqCst) };
            }
        }
        let mut now = &self.list.head;
        loop {
            let mut new = HazPtr {
                ptr: AtomicPtr::new(std::ptr::null_mut()),
                next: AtomicPtr::new(std::ptr::null_mut()),
                flag: AtomicBool::new(false),
            };
            new.next = AtomicPtr::new(now.load(Ordering::SeqCst));
            let boxed = Box::leak(Box::new(new));
            if self
                .list
                .head
                .compare_exchange(
                    now.load(Ordering::Relaxed),
                    boxed,
                    Ordering::SeqCst,
                    Ordering::SeqCst,
                )
                .is_ok()
            {
                return unsafe { &*boxed };
            } else {
                now = &self.list.head;
                let drop = unsafe { Box::from_raw(boxed) };
                std::mem::drop(drop);
                while !current.is_null() {
                    let flag = unsafe { &(*current).flag };
                    if flag
                        .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                        .is_ok()
                    {
                        return unsafe { &(*current) };
                    } else {
                        current = unsafe { (&(*current).next).load(Ordering::SeqCst) };
                    }
                }
            }
        }
    }
}

struct HazPtrs {
    head: AtomicPtr<HazPtr>,
}

struct Retired {
    head: AtomicPtr<Ret>,
}

impl Retired {
    fn reclaim(&mut self) {
        todo!()
    }
}

struct Ret {
    ptr: AtomicPtr<()>,
    next: AtomicPtr<Ret>,
}
