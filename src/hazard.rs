use std::collections::HashSet;
use std::convert::AsRef;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicPtr};

pub(crate) static SHARED_DOMAIN: GlobalDomain = GlobalDomain {
    list: HazardList {
        head: AtomicPtr::new(std::ptr::null_mut()),
    },
    ret: RetiredList {
        head: AtomicPtr::new(std::ptr::null_mut()),
    },
};

#[derive(Default)]
pub struct Holder(Option<&'static Hazard>);

pub struct Guard<'a, T> {
    hazptr: &'static Hazard,
    pub(crate) data: *mut T,
    _marker: PhantomData<&'a T>,
}

impl<T> AsRef<T> for Guard<'_, T> {
    fn as_ref(&self) -> &T {
        &(*self)
    }
}

impl<T> Deref for Guard<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &(*self.data) }
    }
}

///SAFETY:
///  This method can cause safety issues so it must be handled with care.
///  If two threads deref_mut the guard to the same underlying T we will
///  then have two mutable pointers to the same thing. If they are used to
///  read or write at the same time, we will run into undefined behaviour.
impl<T> DerefMut for Guard<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut (*self.data) }
    }
}

impl<T> Drop for Guard<'_, T> {
    fn drop(&mut self) {
        self.hazptr
            .ptr
            .store(std::ptr::null_mut(), Ordering::Release);
        self.hazptr.flag.store(true, Ordering::Release);
    }
}

impl Holder {
    /// SAFETY:
    ///   1. The user must pass a valid pointer. Passing in invalid pointers such as a misaligned
    ///      one will cause undefined behaviour.
    ///   2. If a null pointer is passed that will be taken care of by the implementation as we
    ///      have made sure using NonNull that it does not get dereferenced.
    pub unsafe fn load_pointer<'a, T>(&'a mut self, ptr: &'_ AtomicPtr<T>) -> Option<Guard<'a, T>> {
        let hazptr = if let Some(t) = self.0 {
            t
        } else {
            let ptr = SHARED_DOMAIN.acquire();
            self.0 = Some(ptr);
            ptr
        };
        let mut ptr1 = ptr.load(Ordering::Acquire);
        let ret = loop {
            hazptr.protect(ptr1 as *mut ());
            let ptr2 = ptr.load(Ordering::Acquire);
            if ptr1 == ptr2 {
                if let Some(_) = NonNull::new(ptr1) {
                    let data = ptr1;
                    break Some(Guard {
                        hazptr: &hazptr,
                        data: data,
                        _marker: PhantomData,
                    });
                } else {
                    break None;
                }
            } else {
                ptr1 = ptr2;
            }
        };
        return ret;
    }

    ///SAFETY:
    ///  1. Swap ensures that the old pointer gets retired. The user must make sure that similar to
    ///     the load method, a valid pointer is passed failing which will cause undefined
    ///     behaviour.
    ///  2. Calling the swap method with a retired pointer will cause the retired pointer to be
    ///     retired again which will lead to it being double reclaimed leading to undefined
    ///     behaviour. The user must ensure that this does not happen.
    pub unsafe fn swap<T>(
        &mut self,
        atomic: &'_ AtomicPtr<T>,
        ptr: *mut T,
        deleter: &'static dyn Deleter,
    ) -> Option<DoerWrapper<'_, T>> {
        let current = atomic.swap(ptr, Ordering::AcqRel);
        if current.is_null() {
            return None;
        } else {
            let wrapper = DoerWrapper {
                inner: current,
                domain: &SHARED_DOMAIN,
                deleter: deleter,
            };
            return Some(wrapper);
        }
    }

    ///SAFETY:
    ///  1. This method provides a way to get the wrapper to call the retire method if the user is
    ///     not relying on swap. It must be used with care as repeatedly using load without
    ///     using this method and calling retire on it will lead to memory leaks.
    pub unsafe fn get_wrapper<T>(
        &mut self,
        atomic: &'_ AtomicPtr<T>,
        deleter: &'static dyn Deleter,
    ) -> Option<DoerWrapper<'_, T>> {
        let current = atomic.swap(std::ptr::null_mut(), Ordering::AcqRel);
        if current.is_null() {
            return None;
        } else {
            let wrapper = DoerWrapper {
                inner: current,
                domain: &SHARED_DOMAIN,
                deleter: deleter,
            };
            return Some(wrapper);
        }
    }

    fn get_domain() -> &'static GlobalDomain {
        &SHARED_DOMAIN
    }

    pub fn try_reclaim() {
        let domain = Self::get_domain();
        unsafe {
            domain.ret.reclaim(&domain.list);
        }
    }
}

pub(crate) struct Hazard {
    ptr: AtomicPtr<()>,
    next: AtomicPtr<Hazard>,
    flag: AtomicBool,
}

impl Hazard {
    pub fn protect(&self, ptr: *mut ()) {
        self.ptr.store(ptr, Ordering::SeqCst);
    }
}

pub trait Doer {
    fn domain<'a>(&'a self) -> &'a GlobalDomain;
    fn retire(&mut self);
}

pub struct DoerWrapper<'a, T> {
    pub(crate) inner: *mut T,
    domain: &'a GlobalDomain,
    deleter: &'static dyn Deleter,
}

impl<T> Deref for DoerWrapper<'_, T> {
    type Target = T;
    fn deref(&self) -> &Self::Target {
        unsafe { &(*self.inner) }
    }
}

impl<T> DerefMut for DoerWrapper<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut (*self.inner) }
    }
}

impl<T> Doer for DoerWrapper<'_, T> {
    fn domain<'a>(&'a self) -> &'a GlobalDomain {
        self.domain
    }

    ///SAFETY:
    ///  The user must make sure that a retired pointer is not retired again.
    fn retire(&mut self) {
        if self.inner.is_null() {
            let domain = self.domain();
            unsafe {
                (&domain.ret).reclaim(&domain.list);
            }
            return;
        }
        let domain = self.domain();
        let mut current = (&domain.ret.head).load(Ordering::Acquire);
        loop {
            let ret = Retired {
                ptr: self.inner as *mut dyn Uniform,
                next: AtomicPtr::new(std::ptr::null_mut()),
                deleter: self.deleter,
            };
            ret.next.store(current, Ordering::Release);
            let boxed = Box::into_raw(Box::new(ret));
            if domain
                .ret
                .head
                .compare_exchange(current, boxed, Ordering::AcqRel, Ordering::Relaxed)
                .is_err()
            {
                let drop = unsafe { Box::from_raw(boxed) };
                current = (&domain.ret.head).load(Ordering::Acquire);
                std::mem::drop(drop);
            } else {
                unsafe { (&domain.ret).reclaim(&domain.list) };
                break;
            }
        }
    }
}

pub struct GlobalDomain {
    list: HazardList,
    ret: RetiredList,
}

impl GlobalDomain {
    fn acquire(&self) -> &'static Hazard {
        let mut current = (&self.list.head).load(Ordering::Acquire);
        while !current.is_null() {
            if unsafe { &(*current).flag }
                .compare_exchange(true, false, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return unsafe { &(*current) };
            } else {
                current = unsafe { (&(*current).next).load(Ordering::Acquire) };
            }
        }

        let mut now = self.list.head.load(Ordering::Acquire);
        loop {
            let new = Hazard {
                ptr: AtomicPtr::new(std::ptr::null_mut()),
                next: AtomicPtr::new(std::ptr::null_mut()),
                flag: AtomicBool::new(false),
            };
            new.next.store(now, Ordering::Release);
            let boxed = Box::into_raw(Box::new(new));
            if self
                .list
                .head
                .compare_exchange(now, boxed, Ordering::AcqRel, Ordering::Relaxed)
                .is_ok()
            {
                return unsafe { &*boxed };
            } else {
                let drop = unsafe { Box::from_raw(boxed) };
                std::mem::drop(drop);
                now = self.list.head.load(Ordering::Acquire);
            }
        }
    }
}

pub(crate) struct HazardList {
    head: AtomicPtr<Hazard>,
}

pub struct RetiredList {
    head: AtomicPtr<Retired>,
}

pub trait Uniform {}

impl<T> Uniform for T {}

pub(crate) struct Retired {
    ptr: *mut dyn Uniform,
    next: AtomicPtr<Retired>,
    deleter: &'static dyn Deleter,
}

pub trait Deleter {
    fn delete(&self, ptr: *mut dyn Uniform);
}

/// SAFETY:
///   1. The user would have to pass an instance of one of the two zero sized types defined below:
///     DropBox and DropPointer on the basis of how the actual raw pointer to the underlying type
///     was created. This is necessary because using the drop_in_place() method on every pointer will
///     not dealloate the instance of the box for all those pointers created using Box::into_raw().
///   2. The user must create the instance using static as the trait object must have a static
///      lifetime because we never know when the delete method on that deleter will be called.
///      Using static does not come with any memory overhead as the underlying type would be a zero
///      sized type.
pub struct BoxedPointer;

impl BoxedPointer {
    pub const fn new() -> Self {
        BoxedPointer
    }
}

impl Deleter for BoxedPointer {
    fn delete(&self, ptr: *mut dyn Uniform) {
        if let Some(_) = NonNull::new(ptr) {
            let drop = unsafe { Box::from_raw(ptr) };
            std::mem::drop(drop);
        }
    }
}

pub struct DropPointer;

impl DropPointer {
    pub const fn new() -> Self {
        DropPointer
    }
}

impl Deleter for DropPointer {
    fn delete(&self, ptr: *mut dyn Uniform) {
        if let Some(_) = NonNull::new(ptr) {
            unsafe {
                std::ptr::drop_in_place(ptr);
            }
        }
    }
}

impl RetiredList {
    /// SAFETY:
    ///    The user must make sure that the reclaim method is not called on the list of retired
    ///    pointers contaning two similar pointers as this will lead to the same pointers being
    ///    dereferenced leading to undefined behaviour.
    unsafe fn reclaim<'a>(&self, domain: &'a HazardList) {
        let mut set = HashSet::new();
        let mut swapped = (self.head).swap(std::ptr::null_mut(), Ordering::AcqRel);
        let mut current = (&(domain.head)).load(Ordering::Acquire);
        while !current.is_null() {
            let a = unsafe { (*current).ptr.load(Ordering::Acquire) };
            set.insert(a);
            current = unsafe { (&(*current).next).load(Ordering::Acquire) };
        }
        let mut remaining: *mut Retired = std::ptr::null_mut();
        while !swapped.is_null() {
            let check = unsafe { (*swapped).ptr };
            if !set.contains(&(check as *mut ())) {
                let deleter = unsafe { (*swapped).deleter };
                deleter.delete(check);
                let to_be_dropped = swapped;
                swapped = unsafe { ((*swapped).next).load(Ordering::Acquire) };
                let drop = unsafe { Box::from_raw(to_be_dropped) };
                std::mem::drop(drop);
            } else {
                let next = unsafe { ((*swapped).next).load(Ordering::Acquire) };
                if remaining.is_null() {
                    remaining = swapped;
                    unsafe {
                        (*remaining)
                            .next
                            .store(std::ptr::null_mut(), Ordering::Release);
                    }
                } else {
                    unsafe {
                        (*swapped).next.store(remaining, Ordering::Release);
                    }
                    remaining = swapped;
                }
                swapped = next;
            }
        }
        // we also need to make sure that we take care of all the pointers that have been retired
        // in the meantime..therefore I came up with this solution
        loop {
            if self
                .head
                .compare_exchange(
                    std::ptr::null_mut(),
                    remaining,
                    Ordering::AcqRel,
                    Ordering::Relaxed,
                )
                .is_ok()
            {
                return;
            } else {
                if remaining.is_null() {
                    remaining = self.head.swap(std::ptr::null_mut(), Ordering::AcqRel);
                } else {
                    let mut safety_variable = remaining;
                    while unsafe { !(*safety_variable).next.load(Ordering::Acquire).is_null() } {
                        safety_variable =
                            unsafe { (*safety_variable).next.load(Ordering::Acquire) };
                    }
                    let to_be_swapped = self.head.swap(std::ptr::null_mut(), Ordering::AcqRel);
                    unsafe {
                        (*safety_variable)
                            .next
                            .store(to_be_swapped, Ordering::Release);
                    }
                }
            }
        }
    }
}
