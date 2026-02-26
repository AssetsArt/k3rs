//! VM lifecycle management — start and stop virtual machines.
//!
//! All Virtualization.framework operations must happen on the main thread.
//! We use `dispatch2::run_on_main` for this, following the same pattern
//! as the reference vz project.

use std::process;
use std::sync::Arc;
use std::time::Duration;

use block2::StackBlock;
use dispatch2::{DispatchQueue, DispatchTime, MainThreadBound, run_on_main};
use objc2::rc::Retained;
use objc2_foundation::NSError;
use objc2_virtualization::VZVirtualMachine;
use tracing::{error, info};

use crate::ipc;

/// Clean up IPC socket and exit the process.
fn cleanup_exit(code: i32) -> ! {
    ipc::cleanup();
    process::exit(code);
}

/// Start a VM on the main thread using a completion handler.
pub fn start_vm(vm: Arc<MainThreadBound<Retained<VZVirtualMachine>>>) {
    run_on_main(|marker| {
        info!("starting vm");
        let vm = vm.get(marker);
        let block = &StackBlock::new(|err: *mut NSError| {
            if err.is_null() {
                info!("vm started successfully");
            } else {
                error!("vm failed to start, err={}", unsafe {
                    (*err).localizedDescription()
                });
                cleanup_exit(1);
            }
        });
        unsafe {
            vm.startWithCompletionHandler(block);
        }
    });
}

/// Stop a VM — first try graceful stop, then force after timeout.
pub fn stop_vm(name: &str, vm: Arc<MainThreadBound<Retained<VZVirtualMachine>>>) {
    run_on_main(|marker| {
        info!(name, "stopping vm");
        if request_stop_vm(vm.get(marker)) {
            let vm_clone = Arc::clone(&vm);
            let timeout = DispatchTime::try_from(Duration::from_secs(15)).unwrap();
            let result = DispatchQueue::main().after(timeout, move || force_stop_vm(vm_clone));
            if let Err(err) = result {
                error!("failed to queue force_stop_vm, err={err:?}");
            }
        } else {
            force_stop_vm(vm);
        }
    });
}

fn request_stop_vm(vm: &Retained<VZVirtualMachine>) -> bool {
    unsafe {
        if vm.canRequestStop() {
            info!("requesting vm to stop gracefully");
            if let Err(err) = vm.requestStopWithError() {
                error!(
                    "failed to request vm to stop, err={}",
                    err.localizedDescription()
                );
                cleanup_exit(1);
            }
            return true;
        }
        false
    }
}

fn force_stop_vm(vm: Arc<MainThreadBound<Retained<VZVirtualMachine>>>) {
    run_on_main(|marker| {
        info!("force stopping vm");
        let vm = vm.get(marker);
        if unsafe { vm.canStop() } {
            let block = &StackBlock::new(|err: *mut NSError| {
                if err.is_null() {
                    info!("vm stopped");
                    cleanup_exit(0);
                } else {
                    error!("vm failed to stop, err={}", unsafe {
                        (*err).localizedDescription()
                    });
                    cleanup_exit(1);
                }
            });
            unsafe {
                vm.stopWithCompletionHandler(block);
            }
        } else {
            error!("vm cannot be stopped");
            cleanup_exit(1);
        }
    });
}
