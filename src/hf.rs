use clap::Parser;
use serde::Deserialize;
use serde_json;
use std::sync::Arc;
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use crate::smart_dl;
use crate::config::{AppContext, AppConfig};

#[derive(Parser)]
pub struct Args {
    #[arg(short = 'j', long, help = "huggingface username/repository")]
    pub repo: String,
	
	#[arg(short = 'm', long, default_value = "4", help = "Max parallel downloads")]
    pub max: String,
}

#[derive(Debug, Deserialize)]
pub struct RepoFile {
    pub rfilename: String,
}

pub async fn fetch_huggingface_repo_files(context: Arc<AppContext>, repo: &str, mut multi: Arc<MultiProgress>) -> Result<Vec<RepoFile>, Box<dyn std::error::Error + Send + Sync>> {
    let url = format!("https://huggingface.co/api/models/{}", repo);
    let client = reqwest::Client::new();

    let resp = client
        .get(&url)
        .header("User-Agent", "huggingface-downloader")
        .send()
        .await?
        .json::<serde_json::Value>()
        .await?;

    let files = serde_json::from_value(resp["siblings"].clone())?;
    Ok(files)
}

pub async fn download_repo_files(
	context: Arc<AppContext>,
    client: reqwest::Client,
    repo: &str,
    files: Vec<RepoFile>,
	mut multi: Arc<MultiProgress>
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target_dir = repo.replace("/", "_");

    for f in files {
        let remote_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, f.rfilename);
        let local_path = format!("{}/{}", target_dir, f.rfilename);
		
		if tokio::fs::try_exists(&local_path).await? {
			println!("✅ Skipped (exists): {}", local_path);
			continue;
		}
	
        if let Some(parent) = std::path::Path::new(&local_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }

		let mut max_parallel = 4;
		if let Some(conf) = &context.config {
			max_parallel = conf.max_parallel;
		}
		
        println!("⬇️  Downloading {}", remote_url);
        smart_dl::smart_download(context.clone(), &client, &remote_url, &local_path, max_parallel as usize, Arc::new(tokio::sync::Notify::new()), multi.clone()).await?;
    }

    Ok(())
}
