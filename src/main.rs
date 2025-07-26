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
