use std::collections::HashSet;
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicPtr};

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

    pub fn swap<T>(&mut self, ptr: *mut T, deleter: fn(*mut ())) -> HazPtrObjectWrapper<T> {
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
    fn domain<'a>(&'a self) -> &'a HazPtrDomain;
    fn retire(&mut self, ptr: *mut ());
}

pub struct HazPtrObjectWrapper<'a, T> {
    inner: *mut T,
    domain: &'a HazPtrDomain,
    deleter: fn(*mut ()),
}

impl<T> HazPtrObject for HazPtrObjectWrapper<'_, T> {
    fn domain<'a>(&'a self) -> &'a HazPtrDomain {
        self.domain
    }
    fn retire(&mut self, ptr: *mut ()) {
        let domain = self.domain();
        let current = (&domain.ret.head).load(Ordering::SeqCst);
        loop {
            let ret = Ret {
                ptr: AtomicPtr::new(ptr),
                next: AtomicPtr::new(std::ptr::null_mut()),
                deleter: self.deleter,
            };
            if current.is_null() {
                let boxed = Box::leak(Box::new(ret));
                if domain
                    .ret
                    .head
                    .compare_exchange(
                        std::ptr::null_mut(),
                        boxed,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    )
                    .is_err()
                {
                    let drop = unsafe { Box::from_raw(boxed) };
                    std::mem::drop(drop);
                } else {
                    (&domain.ret).reclaim(&domain.list);
                    break;
                }
            } else {
                ret.next.store(current, Ordering::SeqCst);
                let boxed = Box::leak(Box::new(ret));
                if domain
                    .ret
                    .head
                    .compare_exchange(current, boxed, Ordering::SeqCst, Ordering::SeqCst)
                    .is_err()
                {
                    let drop = unsafe { Box::from_raw(boxed) };
                    std::mem::drop(drop);
                } else {
                    (&domain.ret).reclaim(&domain.list);
                    break;
                }
            }
        }
    }
}

impl<T> Deref for HazPtrObjectWrapper<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &(*self.inner) }
    }
}

pub struct HazPtrDomain {
    list: HazPtrs,
    ret: Retired,
}

impl HazPtrDomain {
    pub fn acquire(&self) -> &'static HazPtr {
        if self.list.head.load(Ordering::SeqCst).is_null() {
            let hazptr = HazPtr {
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

struct Ret {
    ptr: AtomicPtr<()>,
    next: AtomicPtr<Ret>,
    deleter: fn(*mut ()),
}

impl Retired {
    fn reclaim<'a>(&self, domain: &'a HazPtrs) {
        let mut set = HashSet::new();
        let mut current = (&(domain.head)).load(Ordering::SeqCst);
        while !current.is_null() {
            let a = unsafe { (*current).ptr.load(Ordering::SeqCst) };
            set.insert(a);
            current = unsafe { (&(*current).next).load(Ordering::SeqCst) };
        }
        let mut remaining = std::ptr::null_mut();
        let mut now = (self.head).swap(std::ptr::null_mut(), Ordering::SeqCst);
        while !now.is_null() {
            let check = unsafe { ((*now).ptr).load(Ordering::SeqCst) };
            if !set.contains(&(check as *mut ())) {
                let deleter = unsafe { (*now).deleter };
                (deleter)(check);
            } else {
                let next = unsafe { ((*now).next).load(Ordering::SeqCst) };
                unsafe { (*now).next.store(remaining, Ordering::SeqCst) };
                remaining = now;
                now = next;
            }
        }
        self.head.swap(remaining, Ordering::SeqCst);
    }
}
