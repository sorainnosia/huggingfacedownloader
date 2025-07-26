use reqwest::{Client, header::RANGE};
use tokio::{
    fs::{File, remove_file},
    io::AsyncWriteExt,
    sync::{mpsc, Semaphore, Notify},
    signal,
};
use futures_util::StreamExt;
use futures::future::join_all;
use std::sync::Arc;
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use std::{time::Duration};
use uuid;
use clap::Parser;
use serde::Deserialize;
use crate::config::AppContext;
use crate::async_taskwait::AsyncTaskWait;
use crate::async_taskwait;

pub async fn smart_download(
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
    chunk_count: usize,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let head = client.head(url).send().await?;
    let range_ok = head.headers().get("accept-ranges")
        .map_or(false, |v| v == "bytes");

    let total_size = head.headers().get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
	
	let client2 = client.clone();
	let url2 = url.to_string();
	let filename2 = filename.to_string();
	
	context.taskwait.wait_for_slot();
	let handle = tokio::spawn(async move {
		let result = if range_ok && total_size.is_some() {
			download_in_chunks(context.clone(), &client2.clone(), url2.as_str(), filename2.as_str(), total_size.unwrap(), chunk_count, cancel_notify, multi.clone()).await
		} else {
			download_whole_with_progress(context.clone(), &client2.clone(), url2.as_str(), filename2.as_str(), cancel_notify.clone(), multi.clone()).await
		};
	});
	async_taskwait::add_task_handle(handle);
	
	Ok(())
}

pub async fn download_whole_with_progress(
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let response = client.get(url).send().await?;
    let total = response.content_length().unwrap_or(0);

    let bar = multi.add(ProgressBar::new(total));
    bar.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("#>-"));

    let mut stream = response.bytes_stream();
    let mut file = File::create(filename).await?;

    tokio::select! {
        _ = cancel_notify.notified() => {
            println!("🛑 Cancelled while downloading full: {}", url);
            drop(file);
            let _ = remove_file(filename).await;
            bar.finish_and_clear();
            return Ok(());
        }

        _ = async {
            while let Some(chunk) = stream.next().await {
                let data = chunk?;
                file.write_all(&data).await?;
                bar.inc(data.len() as u64);
            }
            bar.finish_with_message("Done");
			bar.finish_and_clear();
            Ok::<(), Box<dyn std::error::Error + Send + Sync>>(())
        } => {}
    }

    Ok(())
}

pub async fn download_in_chunks(
	context: Arc<AppContext>,
    client: &Client,
    url: &str,
    filename: &str,
    total_size: u64,
    chunk_count: usize,
    cancel_notify: Arc<Notify>,
	mut multi: Arc<MultiProgress>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let chunk_size = total_size / chunk_count as u64;
    let results = Arc::new(tokio::sync::Mutex::new(Vec::new()));

    let pb = Arc::new(multi.add(ProgressBar::new(total_size)));
    pb.set_style(ProgressStyle::default_bar()
        .template("{spinner:.green} [{elapsed_precise}] [{bar:40.cyan/blue}] {bytes}/{total_bytes} ({eta})")
        .unwrap()
        .progress_chars("#>-"));

    // ✅ Build a list of async tasks, not using tokio::spawn
    let futures = (0..chunk_count).map(|i| {
        let start = i as u64 * chunk_size as u64;
        let end = if i == chunk_count - 1 {
            total_size - 1
        } else {
            (i as u64 + 1) * chunk_size as u64 - 1
        };

        let url = url.to_string();
        let client = client.clone();
        let results = results.clone();
        let cancel_notify = cancel_notify.clone();
        let pb = pb.clone(); // Arc<ProgressBar>

        async move {
            let range = format!("bytes={}-{}", start, end);
            let req = client.get(&url).header(RANGE, range);

            tokio::select! {
                _ = cancel_notify.notified() => {
                    println!("🛑 Chunk {} canceled", i);
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

                    let mut buf = Vec::new();

                    while let Some(chunk) = resp.chunk().await.unwrap() {
                        pb.inc(chunk.len() as u64);
                        buf.extend_from_slice(&chunk);
                    }

                    let mut lock = results.lock().await;
                    lock.push((start, buf));
                    println!("📦 Chunk {} done", i);
                }
            }
        }
    });

    // ✅ Await all without spawn (no Send requirement)
    join_all(futures).await;
    pb.finish_with_message("Done");
	pb.finish_and_clear();

    let mut sorted = Arc::try_unwrap(results).unwrap().into_inner();
    sorted.sort_by_key(|(start, _)| *start);

    let mut file = File::create(filename).await?;
    for (_start, data) in sorted {
        file.write_all(&data).await?;
    }

    Ok(())
}
