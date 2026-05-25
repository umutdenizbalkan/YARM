// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Umut Deniz Balkan

#![no_std]

#[macro_export]
macro_rules! install {
    ($heap_size:expr, $oom_message:expr) => {
        use core::alloc::{GlobalAlloc, Layout};
        use core::cell::UnsafeCell;
        use core::mem::{align_of, size_of};
        use core::ptr::null_mut;
        use core::sync::atomic::{AtomicBool, Ordering};
        const HEAP_SIZE: usize = $heap_size;
        #[repr(C)] struct BlockHeader { size: usize, next: *mut BlockHeader }
        const HEADER_SIZE: usize = size_of::<BlockHeader>();
        const MIN_SPLIT: usize = HEADER_SIZE + align_of::<usize>();
        struct A { heap: UnsafeCell<[u8; HEAP_SIZE]>, free: UnsafeCell<*mut BlockHeader>, init: AtomicBool, lock: AtomicBool }
        unsafe impl Sync for A {}
        static ALLOC: A = A { heap: UnsafeCell::new([0; HEAP_SIZE]), free: UnsafeCell::new(null_mut()), init: AtomicBool::new(false), lock: AtomicBool::new(false) };
        #[inline] const fn align_up(v: usize, a: usize) -> usize { (v + (a - 1)) & !(a - 1) }
        impl A {
            fn lk(&self){ while self.lock.compare_exchange_weak(false,true,Ordering::Acquire,Ordering::Relaxed).is_err(){ core::hint::spin_loop(); } }
            fn ul(&self){ self.lock.store(false,Ordering::Release); }
            unsafe fn ensure(&self){ if self.init.load(Ordering::Acquire){return;} let b=self.heap.get().cast::<u8>() as usize; let s=align_up(b,align_of::<BlockHeader>()); let off=s-b; if off+HEADER_SIZE>HEAP_SIZE { *self.free.get()=null_mut(); self.init.store(true,Ordering::Release); return; } let h=s as *mut BlockHeader; (*h).size=HEAP_SIZE-off; (*h).next=null_mut(); *self.free.get()=h; self.init.store(true,Ordering::Release); }
            unsafe fn alloc(&self,l:Layout)->*mut u8{ self.ensure(); let mut p=null_mut(); let mut c=*self.free.get(); let ra=l.align().max(align_of::<usize>()); while !c.is_null(){ let bs=c as usize; let ps=match bs.checked_add(HEADER_SIZE).map(|x|align_up(x,ra)){Some(v)=>v,None=>return null_mut()}; let pe=match ps.checked_add(l.size()){Some(v)=>v,None=>return null_mut()}; let be=match bs.checked_add((*c).size){Some(v)=>v,None=>return null_mut()}; if pe<=be{ let rem=be-pe; let n=(*c).next; if rem>=MIN_SPLIT{ let nf=pe as *mut BlockHeader; (*nf).size=rem; (*nf).next=n; if p.is_null(){*self.free.get()=nf}else{(*p).next=nf}; (*c).size=pe-bs; } else if p.is_null(){*self.free.get()=n}else{(*p).next=n}; return ps as *mut u8; } p=c; c=(*c).next; } null_mut() }
            unsafe fn dealloc(&self,ptr:*mut u8){ if ptr.is_null(){return;} self.ensure(); let b=(ptr as usize).saturating_sub(HEADER_SIZE) as *mut BlockHeader; let mut p=null_mut(); let mut c=*self.free.get(); while !c.is_null() && (c as usize)<(b as usize){ p=c; c=(*c).next; } (*b).next=c; if p.is_null(){*self.free.get()=b}else{(*p).next=b}; self.coalesce(b); if !p.is_null(){ self.coalesce(p); }}
            unsafe fn coalesce(&self,b:*mut BlockHeader){ let n=(*b).next; if n.is_null(){return;} if (b as usize).saturating_add((*b).size)==n as usize { (*b).size=(*b).size.saturating_add((*n).size); (*b).next=(*n).next; } }
        }
        struct G;
        unsafe impl GlobalAlloc for G {
            unsafe fn alloc(&self, layout: Layout) -> *mut u8 { if layout.size()==0 { return layout.align() as *mut u8; } ALLOC.lk(); let p=unsafe{ALLOC.alloc(layout)}; ALLOC.ul(); p }
            unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) { if ptr.is_null()||layout.size()==0{return;} ALLOC.lk(); unsafe{ALLOC.dealloc(ptr)}; ALLOC.ul(); }
        }
        #[global_allocator] static RUNTIME_GLOBAL_ALLOCATOR: G = G;
        #[unsafe(no_mangle)] extern "C" fn __rust_alloc_error_handler(_size: usize, _align: usize) -> ! { panic!($oom_message) }
    };
}

#[cfg(test)]
mod tests { extern crate std; use std::alloc::{alloc,dealloc,Layout};
#[test] fn alloc_free_reuse_same_block(){ let l=Layout::from_size_align(64,8).unwrap(); let a=unsafe{alloc(l)}; assert!(!a.is_null()); unsafe{dealloc(a,l)}; let b=unsafe{alloc(l)}; assert_eq!(a,b); unsafe{dealloc(b,l)}; }
#[test] fn alloc_many_free_many_reuse(){ let s=Layout::from_size_align(1024,8).unwrap(); let mut v=std::vec::Vec::new(); for _ in 0..64{ let p=unsafe{alloc(s)}; assert!(!p.is_null()); v.push(p);} for p in v{ unsafe{dealloc(p,s)};} let l=Layout::from_size_align(48*1024,8).unwrap(); let p=unsafe{alloc(l)}; assert!(!p.is_null()); unsafe{dealloc(p,l)}; }
#[test] fn alignment_is_respected(){ for a in [1,8,16,64]{ let l=Layout::from_size_align(96,a).unwrap(); let p=unsafe{alloc(l)}; assert_eq!((p as usize)%a,0); unsafe{dealloc(p,l)}; } }
#[test] fn split_and_coalesce(){ let l=Layout::from_size_align(4096,16).unwrap(); let a=unsafe{alloc(l)}; let b=unsafe{alloc(l)}; let c=unsafe{alloc(l)}; unsafe{dealloc(b,l)}; let s=Layout::from_size_align(1024,16).unwrap(); let b2=unsafe{alloc(s)}; unsafe{dealloc(a,l);dealloc(b2,s);dealloc(c,l);} let big=Layout::from_size_align(14*1024,16).unwrap(); let p=unsafe{alloc(big)}; assert!(!p.is_null()); unsafe{dealloc(p,big)}; }
#[test] fn pm_like_large_temp_sequence(){ for size in [85*1024,95*1024,84*1024]{ let l=Layout::from_size_align(size,16).unwrap(); let p=unsafe{alloc(l)}; assert!(!p.is_null()); unsafe{dealloc(p,l)}; } }
}
