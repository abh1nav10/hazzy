#[cfg(test)]
mod hazard_test {
    use hazzy::{DropBox, HazPtrHolder, HazPtrObject};
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
        let atm_ptr = AtomicPtr::new(boxed1);
        let mut holder = HazPtrHolder::default();
        let guard = unsafe { holder.load(&atm_ptr) };
        static DROPBOX: DropBox = DropBox::new();
        std::mem::drop(guard);
        if let Some(mut wrapper) = unsafe { holder.swap(&atm_ptr, boxed2, &DROPBOX) } {
            wrapper.retire();
        }
        assert_eq!(check.get_number_of_drops(), 1 as usize);
        let _ = unsafe { Box::from_raw(boxed2) };
        std::mem::drop(check);
    }
}
