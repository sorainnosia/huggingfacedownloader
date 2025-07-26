// async_taskwait.rs
use std::sync::{Arc, Mutex};
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use futures::future::join_all;

lazy_static! {
    pub static ref ISRUNNING: Arc<Mutex<bool>> = Arc::new(Mutex::new(true));
    pub static ref TASK_HANDLES: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));
}

#[derive(Debug)]
pub struct AsyncTaskWait {
    semaphore: Arc<Semaphore>,
    max_parallel: usize,
}

impl AsyncTaskWait {
    pub fn new(max_parallel: i32) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(max_parallel as usize)),
            max_parallel: max_parallel as usize,
        }
    }

    pub async fn acquire(&self) -> Result<tokio::sync::SemaphorePermit<'_>, tokio::sync::AcquireError> {
        self.semaphore.acquire().await
    }

    pub fn available_permits(&self) -> usize {
        self.semaphore.available_permits()
    }

    pub async fn wait_for_slot(&self) -> bool {
        let is_running = {
            *ISRUNNING.lock().unwrap()
        };
        
        if !is_running {
            return false;
        }

        match self.acquire().await {
            Ok(permit) => {
                // Return the permit immediately as the caller will hold it
                std::mem::forget(permit);
                true
            }
            Err(_) => false,
        }
    }
}

pub async fn wait_all_tasks() {
    let handles = {
        let mut handles_guard = TASK_HANDLES.lock().unwrap();
        let handles = handles_guard.drain(..).collect::<Vec<_>>();
        handles
    };
    
    join_all(handles).await;
}

pub fn add_task_handle(handle: JoinHandle<()>) {
    let mut handles = TASK_HANDLES.lock().unwrap();
    handles.push(handle);
}

pub fn is_running() -> bool {
    *ISRUNNING.lock().unwrap()
}

pub fn set_running(running: bool) {
    *ISRUNNING.lock().unwrap() = running;
}