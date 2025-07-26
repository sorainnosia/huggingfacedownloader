#[macro_use]
extern crate lazy_static;

use reqwest::{Client, header::RANGE};
use scraper::{Html, Selector};
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

mod config;
mod smart_dl;
mod async_taskwait;
mod hf;
use hf::*;
use crate::smart_dl::*;
use crate::config::*;
use crate::async_taskwait::AsyncTaskWait;

#[derive(Parser)]
pub struct Args {
    #[arg(short = 'j', long, help = "huggingface username/repository")]
    pub repo: String,
	
	#[arg(short = 'm', long, default_value = "4", help = "Max parallel downloads")]
    pub max_parallel: u32,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();

	let mut ctx = AppContext::new();
	ctx.run();
    config::set_config(&mut ctx);
	if let Some(mut c) = ctx.config {
		c.max_parallel = args.max_parallel;
		ctx.taskwait = Arc::new(AsyncTaskWait::new(c.max_parallel as i32));
		ctx.config = Some(c);
	}

	let mut context = Arc::new(ctx);
    let repo = args.repo;

	let m = Arc::new(MultiProgress::new());
    let files = hf::fetch_huggingface_repo_files(context.clone(), &repo, m.clone()).await?;
    let client = reqwest::Client::new();
    hf::download_repo_files(context.clone(), client, &repo, files, m.clone()).await?;

	async_taskwait::wait_all_tasks().await;
    Ok(())
}


// #[tokio::main]
// async fn main() {
    // let base_url = "https://example.com?page=";
    // let max_pending = 10;
    // let max_parallel = 3;
    // let chunk_per_file = 4;

    // let (tx, rx) = mpsc::channel::<String>(max_pending);
    // let client = Client::new();
    // let cancel_notify = Arc::new(Notify::new());
    // let semaphore = Arc::new(Semaphore::new(max_parallel));

    // // Handle Ctrl+C
    // {
        // let cancel_notify = cancel_notify.clone();
        // tokio::spawn(async move {
            // signal::ctrl_c().await.unwrap();
            // println!("\n🛑 Ctrl+C received!");
            // cancel_notify.notify_waiters();
        // });
    // }

    // // ✅ Start the downloader in a task
    // let downloader_task = {
        // let client = client.clone();
        // let cancel_notify = cancel_notify.clone();
        // let semaphore = semaphore.clone();
        // tokio::spawn(download_worker(rx, client, cancel_notify, semaphore, chunk_per_file))
    // };

    // // ✅ Run the crawler directly here (no spawn!)
    // run_crawler_loop(base_url, client.clone(), tx.clone(), cancel_notify.clone()).await;

    // downloader_task.await.unwrap();
// }

// async fn run_crawler_loop(
    // base_url: &str,
    // client: Client,
    // tx: mpsc::Sender<String>,
    // cancel_notify: Arc<Notify>,
// ) {
    // let selector = Selector::parse("a").unwrap();
    // let mut page = 1;

    // loop {
        // // Wait if too many queued
        // if tx.capacity() == 0 {
            // tokio::select! {
                // _ = cancel_notify.notified() => break,
                // _ = tokio::time::sleep(Duration::from_millis(500)) => continue,
            // }
        // }

        // let page_url = format!("{}{}", base_url, page);
        // println!("🌐 Crawling: {}", page_url);

        // match client.get(&page_url).send().await {
            // Ok(resp) => {
                // if let Ok(text) = resp.text().await {
                    // let document = Html::parse_document(&text);
                    // for el in document.select(&selector) {
                        // if let Some(href) = el.value().attr("href") {
                            // if href.starts_with("http") {
                                // if tx.send(href.to_string()).await.is_err() {
                                    // return;
                                // }
                            // }
                        // }
                    // }
                // }
            // }
            // Err(e) => {
                // eprintln!("❌ Crawl error: {}", e);
                // break;
            // }
        // }

        // page += 1;

        // tokio::select! {
            // _ = cancel_notify.notified() => break,
            // _ = tokio::time::sleep(Duration::from_millis(300)) => {}
        // }
    // }

    // println!("🛑 Crawler exiting");
// }

// async fn download_worker(
    // mut rx: mpsc::Receiver<String>,
    // client: Client,
    // cancel_notify: Arc<Notify>,
    // semaphore: Arc<Semaphore>,
    // chunk_count: usize,
// ) {
    // while let Some(link) = rx.recv().await {
        // let permit = semaphore.clone().acquire_owned().await.unwrap();
        // let client = client.clone();
        // let cancel_notify = cancel_notify.clone();

        // tokio::spawn(async move {
            // let filename = format!("file_{}.bin", uuid::Uuid::new_v4());

            // let result = smart_dl::smart_download(&client, &link, &filename, chunk_count, cancel_notify).await;

            // if let Err(e) = result {
                // println!("❌ Download error: {} - {}", link, e);
                // let _ = remove_file(&filename).await;
            // }

            // drop(permit);
        // });
    // }

    // println!("📥 All links processed. Downloader exiting.");
// }