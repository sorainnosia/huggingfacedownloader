use reqwest::{Client, header::RANGE};
use tokio::{
    fs::{File, remove_file},
    io::AsyncWriteExt,
    sync::{mpsc, Notify},
    signal,
};
use futures_util::StreamExt;
use futures::future::join_all;
use std::sync::{Arc, Mutex};
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use std::{time::Duration};
use uuid;
use clap::Parser;
use serde::Deserialize;
use tokio::fs;
use std::path::{PathBuf, Path};
use tokio::io::AsyncReadExt;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::task::JoinHandle;
use tokio::sync::{Semaphore, OwnedSemaphorePermit};
use std::pin::Pin;
use futures::future::ready;
use std::future::Future;
use crate::config::AppContext;
use crate::taskwait;
use crate::async_taskwait::AsyncTaskWait;

static CANCELLED: AtomicBool = AtomicBool::new(false);
static CANCEL_CHECK: u32 = 100;
static BUFFER_SIZE: usize = 204800;

pub async fn smart_download(
	permit: Option<OwnedSemaphorePermit>,
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
	max_try: u32,
	parallel_count: usize,
    chunk_count: usize,
	max_size: Option<u64>,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>,
	resumable: bool
) -> JoinHandle<()> {
    let head = client.head(url).send().await.unwrap();
    let range_ok = head.headers().get("accept-ranges")
        .map_or(false, |v| v == "bytes");

    let total_size = head.headers().get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
	
	let client2 = client.clone();
	let url2 = url.to_string();
	let filename2 = filename.to_string();
	let max_size2 = max_size.clone();
	
	let handle = tokio::spawn(async move {
		let permit2 = permit;
		let result = if range_ok && total_size.is_some() {
			download_in_chunks(context.clone(), &client2.clone(), url2.as_str(), filename2.as_str(), total_size.unwrap(), max_try, parallel_count, chunk_count, max_size2, cancel_notify, multi.clone(), resumable).await
		} else {
			download_whole_with_progress(context.clone(), &client2.clone(), url2.as_str(), filename2.as_str(), max_try, cancel_notify.clone(), multi.clone(), resumable).await
		};
		
		if permit2.is_none() {
			taskwait::new_thread_available();
		}
	});
	
	handle
}

pub async fn is_cancel() -> Result<(), Box<dyn std::error::Error + Send + Sync>> { 
	if CANCELLED.load(Ordering::SeqCst) {
		return Err(Box::<dyn std::error::Error + Send + Sync>::from("Error"));
	}
	return Ok(());
}

fn temp_chunk_path(base: &str, index: usize) -> PathBuf {
    let result = PathBuf::from(base);
	let stem = Path::new(base)
		.file_stem()
		.and_then(|s| s.to_str())
		.unwrap_or("");
		
	if let Some(parent) = result.parent() {
		let new_tmp_dir = parent.join("tmp");
		std::fs::create_dir_all(&new_tmp_dir);
		let ret = new_tmp_dir.join(format!("{}.tmp{}", stem, index));
		return ret;
	} else {
		let new_tmp_dir = result.join("tmp");
		std::fs::create_dir_all(&new_tmp_dir);
		let ret = new_tmp_dir.join(format!("{}.tmp{}", stem, index));
		return ret;
	}
}

pub fn slice_chunk(total_size: u64, chunk_count: usize, start2: u64) -> Vec<(u64, u64, u64)> {
	let max_size: Option<u64> = None;
	let mut result = vec![];
	let chunk_size = total_size / chunk_count as u64;
	for i in 0..chunk_count {
		let mut start = i as u64 * chunk_size;
		let mut end = if i == chunk_count - 1 {
			total_size - 1
		} else {
			(i as u64 + 1) * chunk_size - 1
		};
		
		let length = (end - start) + 1;
		if let Some(m) = &max_size {
			if end + 1 > start {
				if length > *m {
					let temps = slice_chunk(length, chunk_count, start + start2);
					result.extend(temps);
				} else {
					result.push((start + start2, end + start2, length));
				}
			} else {
				result.push((start + start2, end + start2, length));
			}
		} else {
			result.push((start + start2, end + start2, length));
		}
	}
	return result;
}


pub fn slice_chunk_size(total_size: u64, max_size: Option<u64>) -> Vec<(u64, u64, u64)> {
	let mut result = vec![];
	if let Some(maxsize) = max_size {
		let chunk_count = total_size / maxsize as u64;
		let chunk_count_f = total_size as f64 / maxsize as f64;
		
		if total_size <= maxsize as u64 {
			result.push((0, total_size - 1, total_size));
		} else {
			for i in 0..chunk_count {
				let mut start = i as u64 * maxsize;
				let mut end = if i == chunk_count - 1 {
					total_size - 1
				} else {
					(i as u64 + 1) * maxsize - 1
				};
				
				result.push((start, end, maxsize));
			}
			if (chunk_count as f64) < chunk_count_f {
				let remain = total_size - (chunk_count * chunk_count);
				result.push((chunk_count * maxsize, total_size - 1, remain));
			}
		}
	}
	return result;
}

pub async fn download_whole_with_progress(
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
	max_try: u32,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>,
	resumable: bool
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let response = client.get(url).send().await?;
    let total = response.content_length().unwrap_or(0);

    let pb = Arc::new(multi.add(ProgressBar::new(total)));
	let pb2 = pb.clone();
    
	pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {prefix:.bold}")
        .unwrap()
        .progress_chars("#>-"));
		
	let name_only = Path::new(filename)
		.file_name()             // Option<&OsStr>
		.and_then(|s| s.to_str()) // Option<&str>
		.unwrap_or("unknown");
	pb.set_prefix(format!("{}", name_only));
    
	let mut stream = response.bytes_stream();
	let filename2 = filename.to_string();
	
	tokio::spawn({
		let cancel_notify = cancel_notify.clone();
		
		async move {
			signal::ctrl_c().await.ok();
			CANCELLED.store(true, Ordering::SeqCst);
			cancel_notify.notify_waiters();
			*taskwait::ISRUNNING.lock().unwrap() = false;
			let _ = fs::remove_file(&filename2).await;
			
			eprintln!("🧹 Cancelled by Ctrl+C. Temporary files cleaned.");
		}
	});
	
	let mut max_try2 = max_try;
	while max_try2 > 0 {
		let mut success = false;
		tokio::select! {
			_ = cancel_notify.notified() => {
				println!("🛑 Cancelled while downloading full: {}", url);
				let _ = fs::remove_file(filename).await;
				
				return Ok(());
			}

			_ = async {
				let mut file = File::create(filename).await?;
				let mut counter = 0;
				
				while let Some(chunk) = stream.next().await {
					let data = chunk?;
					file.write_all(&data).await?;
					pb2.inc(data.len() as u64);
					
					counter = counter + 1;
					if counter % CANCEL_CHECK == 0 {
						if CANCELLED.load(Ordering::SeqCst) {
							*taskwait::ISRUNNING.lock().unwrap() = false;
							drop(file);
							let _ = fs::remove_file(&filename).await;
							break;
						} else {
							counter = 0;
						}
					}
				}
				if CANCELLED.load(Ordering::SeqCst) == false {
					success = true;
				}
				
				Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
			} => {}
		}
		
		max_try2 -= 1;
		
		if success {
			break;
		} else {
			pb2.set_position(0);
		}
	}
	
	if CANCELLED.load(Ordering::SeqCst) == false {
		pb.finish_with_message(format!("Downloaded {}", url));
		Ok(())
	} else {
		return Err("Download cancelled".into());
	}
}

pub async fn download_in_chunks(
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
    total_size: u64,
	max_try: u32,
	max_parallel: usize,
    chunk_count: usize,
	max_size: Option<u64>,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>,
	resumable: bool
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
	if CANCELLED.load(Ordering::SeqCst) {
		return Err("Download cancelled".into());
	}
	
	let chunk_size = total_size / chunk_count as u64;
	let mut slices = vec![];
	if max_size.is_none() {
		slices = slice_chunk(total_size, chunk_count, 0);
	} else {
		slices = slice_chunk_size(total_size, max_size);
	}
	let chunk_count_real = slices.len();
	let mut tmp_paths = Vec::new();

	let pb = Arc::new(multi.add(ProgressBar::new(total_size)));
	
	pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta}) {prefix:.bold}")
        .unwrap()
        .progress_chars("#>-"));

	let name_only = Path::new(filename)
		.file_name()
		.and_then(|s| s.to_str())
		.unwrap_or("unknown");
	pb.set_prefix(format!("{}", name_only));

	let signal_set = Arc::new(cancel_notify.clone());
	let tmp_dir = PathBuf::from(filename)
		.parent()
		.map(Path::to_path_buf)
		.unwrap_or_else(|| PathBuf::from("."));
	let tmp_filename_base = PathBuf::from(filename).file_name().unwrap().to_str().unwrap().to_string();

	let tmp_cleanup = tmp_paths.clone();
	
	tokio::spawn({
		let cancel_notify = cancel_notify.clone();
		let tmp_filename_base = tmp_filename_base.clone();
		let tmp_dir = tmp_dir.clone();
		
		async move {
			signal::ctrl_c().await.ok();
			CANCELLED.store(true, Ordering::SeqCst);
			cancel_notify.notify_waiters();
			*taskwait::ISRUNNING.lock().unwrap() = false;

			if resumable == false {
				for i in 0..chunk_count_real {
					let path = tmp_dir.join(format!("{}.tmp{}", tmp_filename_base, i));
					if let Ok(metadata) = std::fs::metadata(&path) {
						let _ = fs::remove_file(&path).await;
					}
				}
			}

			eprintln!("🧹 Cancelled by Ctrl+C. Temporary files cleaned.");
		}
	});

	let pb2 = pb.clone();
	let pb3 = pb2.clone();
	let wait = Arc::new(AsyncTaskWait::new(chunk_count as i32));
	
	let futures = (0..slices.len()).map(|i| {
		let sl = slices.clone();
		let start = sl[i].0;
		let end = sl[i].1;
		let length = sl[i].2;
		
		let pb4 = pb3.clone();

		let url = url.to_string();
		let client = client.clone();
		let cancel_notify = cancel_notify.clone();		
		let wait2 = wait.clone();
		let tmp_path = temp_chunk_path(filename, i);
		tmp_paths.push(tmp_path.clone());
		
		async move {
			let permit = wait2.acquire().await;
			
			let mut skip_exist = false;
			let mut size = 0;
			if let Ok(metadata) = std::fs::metadata(&tmp_path) {
				size = metadata.len();
				if size == length {
					skip_exist = true;
				}
			}
			
			if skip_exist == false {
				let mut max_try2 = max_try;
				
				while max_try2 > 0 {
					let client2 = client.clone();
					_ = fs::remove_file(&tmp_path).await;
					
					let range = format!("bytes={}-{}", start, end);
					let req = client2.get(&url).header(RANGE, range);
					let mut success = false;
					tokio::select! {
						_ = cancel_notify.notified() => {
							eprintln!("🛑 Chunk {} canceled", i);
							return;
						}
						
						res = req.send() => {
							let mut resp = match res {
								Ok(r) => r,
								Err(e) => {
									eprintln!("❌ Chunk {} failed to start: {}", i, e);
									return;
								}
							};

							let mut counter = 0;
							let mut file = File::create(&tmp_path).await.unwrap();
							while let Some(chunk) = resp.chunk().await.unwrap() {
								pb4.inc(chunk.len() as u64);
								file.write_all(&chunk).await.unwrap();
								
								counter += 1;
								if counter % CANCEL_CHECK == 0  {
									if CANCELLED.load(Ordering::SeqCst) {
										*taskwait::ISRUNNING.lock().unwrap() = false;
										drop(file);
										let _ = fs::remove_file(&tmp_path).await;
										break;
									} else {
										counter = 0;
									}
								}
							}
							if CANCELLED.load(Ordering::SeqCst) == false {
								success = true;
							}
						}
					}
					
					max_try2 -= 1;
			
					if success {
						break;
					} else {
						pb4.set_position(0);
					}
				}
			} else {
				pb4.inc(size as u64);
			}
		}
	});

	join_all(futures).await;

	if CANCELLED.load(Ordering::SeqCst) {
		return Err("Download cancelled".into());
	}

	pb2.finish_with_message(format!("Downloaded {}", url)); 
	
	let mut output = File::create(filename).await?;
	let mut success = false;
	for i in 0..chunk_count_real {
		let tmp_path = temp_chunk_path(filename, i);
		let mut tmp_file = File::open(&tmp_path).await?;
		let mut buffer = [0u8; BUFFER_SIZE];
		let mut counter = 0;
		
		loop {
			let n = tmp_file.read(&mut buffer).await?;
			if n == 0 {
				break;
			}
			output.write_all(&buffer[..n]).await?;
			
			counter += 1;
			if counter % CANCEL_CHECK == 0 {
				if CANCELLED.load(Ordering::SeqCst) {
					break;
				} else {
					counter = 0;
				}
			}
		}
		
		if CANCELLED.load(Ordering::SeqCst) {
			*taskwait::ISRUNNING.lock().unwrap() = false;
			drop(output);
			let _ = fs::remove_file(&filename).await;
			break;
		}
	}
	
	if CANCELLED.load(Ordering::SeqCst) == false {
		for i in 0..chunk_count_real {
			let path = temp_chunk_path(filename, i);
			let _ = fs::
			remove_file(&path).await;
		}
		let result = PathBuf::from(filename);
			
		if let Some(parent) = result.parent() {
			let new_tmp_dir = parent.join("tmp");
			std::fs::remove_dir(&new_tmp_dir);
		}
		
		return Ok(());
	} else {
		return Err("Download cancelled".into());
	}
}