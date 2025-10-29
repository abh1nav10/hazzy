#[cfg(test)]
mod hazard_test {
    use hazzy::{BoxedPointer, Doer, Holder};
    use std::sync::Arc;
    use std::sync::atomic::Ordering;
    use std::sync::atomic::{AtomicPtr, AtomicUsize};
    struct CountDrops(Arc<AtomicUsize>);
    impl Drop for CountDrops {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
    }
    impl CountDrops {
        fn get_number_of_drops(&self) -> usize {
            self.0.load(Ordering::Relaxed)
        }
    }
    #[test]
    fn test_hazard() {
        let new = Arc::new(AtomicUsize::new(0));
        let check = CountDrops(new.clone());
        let value1 = CountDrops(new.clone());
        let value2 = CountDrops(new.clone());
        let boxed1 = Box::into_raw(Box::new(value1));
        let boxed2 = Box::into_raw(Box::new(value2));
        let ptr1 = AtomicPtr::new(boxed1);
        let mut holder = Holder::default();
        let guard = unsafe { holder.load_pointer(&ptr1) };
        static DROPBOX: BoxedPointer = BoxedPointer::new();
        std::mem::drop(guard);
        if let Some(mut wrapper) = unsafe { holder.swap(&ptr1, boxed2, &DROPBOX) } {
            wrapper.retire();
        }
        assert_eq!(check.get_number_of_drops(), 1 as usize);
        let ptr2 = AtomicPtr::new(boxed2);
        let value3 = CountDrops(new.clone());
        let boxed3 = Box::into_raw(Box::new(value3));
        if let Some(mut wrapper) = unsafe { holder.swap(&ptr2, boxed3, &DROPBOX) } {
            wrapper.retire();
        }
        assert_eq!(check.get_number_of_drops(), 2 as usize);
        let _ = unsafe { Box::from_raw(boxed3) };
    }
}
