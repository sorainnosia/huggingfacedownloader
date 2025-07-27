#![allow(unused)]
use std::sync::{ Arc, Mutex };
use std::thread;
use std::time;
use std::time::Duration;
use tokio::task::JoinHandle;
use futures::future::join_all;

lazy_static! {
    pub static ref ISRUNNING: Arc<Mutex<bool>> = Arc::new(Mutex::new(false));
    pub static ref RUNNING_THREAD: Mutex<i32> = Mutex::new(0);
	pub static ref TASK_HANDLES: Arc<Mutex<Vec<JoinHandle<()>>>> = Arc::new(Mutex::new(Vec::new()));	
}

pub fn is_running() -> bool {
    let isrunning;
    {
        let r = *ISRUNNING.lock().unwrap();
        isrunning = r;
    }
    return isrunning;
}

pub fn wait_available_thread(parallel: i32) -> bool {
    let mut run;
    loop {
        {
            run = *RUNNING_THREAD.lock().unwrap();
        
            if run < parallel {
                *RUNNING_THREAD.lock().unwrap() = run + 1;
                break;
            }
        }
        if is_running() == false { break; }
        thread::sleep(time::Duration::from_millis(300));
    }
    if is_running() == false { return false; }
    return true;
}

pub fn new_thread_available() {
    let run;
    {
        run = *RUNNING_THREAD.lock().unwrap();
        *RUNNING_THREAD.lock().unwrap() = run - 1;
    }
}

pub fn wait_all_thread_done() -> bool {
    loop {
        if is_running() == false { return false; }
        let mut r;
        {
            r = *RUNNING_THREAD.lock().unwrap();
        }
        if r == 0 {
            thread::sleep(Duration::from_millis(500));
            {
                r = *RUNNING_THREAD.lock().unwrap();
            }
            if r == 0 {
                return true;
            }
            if is_running() == false { return false; }
        }
        thread::sleep(Duration::from_millis(500));
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
