use reqwest::{Client};
use clap::Parser;
use serde::Deserialize;
use serde_json;
use std::sync::Arc;
use indicatif::{ProgressBar, ProgressStyle, MultiProgress};
use sha1::Sha1;
use sha2::{Sha256, Digest};
use tokio::fs;
use std::collections::HashMap;
use crate::smart_dl;
use crate::taskwait;
use crate::async_taskwait::AsyncTaskWait;
use crate::config::{AppContext, AppConfig};
use tokio::fs::File;
use tokio::io::{AsyncReadExt, BufReader};
use tokio::sync::{Semaphore, OwnedSemaphorePermit};
use std::path::Path;

#[derive(Debug, Deserialize)]
pub struct RepoFile {
    pub rfilename: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lfs: Option<LfsInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct LfsInfo {
    pub oid: String,  // SHA256 hash for LFS files
    pub size: i64,
    #[serde(rename = "pointerSize")]
    pub pointer_size: i32,
}

#[derive(Debug)]
pub struct FileVerificationResult {
    pub filename: String,
    pub expected_hash: String,
    pub actual_hash: String,
    pub passed: bool,
}

// Add struct for API response parsing
#[derive(Debug, Deserialize)]
struct ApiFileInfo {
    #[serde(rename = "type")]
    file_type: String,
    path: String,
    size: Option<i64>,
    lfs: Option<LfsInfo>,
}

pub async fn fetch_huggingface_repo_files(context: Arc<AppContext>, client: &Client, repo: &str, mut multi: Arc<MultiProgress>) -> Result<Vec<RepoFile>, Box<dyn std::error::Error + Send + Sync>> {
    let mut repo_type = "models".to_string();
    if let Some(c) = &context.config {
        repo_type = c.repo_type.to_string();
    }
    
    let mut all_files = Vec::new();
    let mut path_param = String::new();
    let mut has_more = true;
    let mut page_count = 0;
    
    while has_more {
        // Construct URL with pagination support
        let url = if path_param.is_empty() {
            format!("https://huggingface.co/api/{}/{}/tree/main", repo_type, repo)
        } else {
            format!("https://huggingface.co/api/{}/{}/tree/main?path={}", repo_type, repo, path_param)
        };
        
        let resp = client
            .get(&url)
            .header("User-Agent", "huggingface-downloader")
            .send()
            .await?;
            
        if !resp.status().is_success() {
            // If we get 404, it might mean the repository doesn't exist or is private
            if resp.status().as_u16() == 404 {
                return Err(format!("Repository not found or private: {}/{}", repo_type, repo).into());
            }
            return Err(format!("Failed to fetch repo files: HTTP {}", resp.status()).into());
        }
        
        let json_resp = resp.json::<Vec<ApiFileInfo>>().await?;
        
        // Check if we got any results
        if json_resp.is_empty() {
            has_more = false;
            continue;
        }
        
        // Process files and folders
        let mut folders_to_explore = Vec::new();
        
        for item in json_resp {
            match item.file_type.as_str() {
                "file" => {
                    // Add file to our list
                    all_files.push(RepoFile {
                        rfilename: item.path.clone(),
                        lfs: item.lfs,
                        size: item.size,
                    });
                }
                "directory" => {
                    // Add folder to explore
                    folders_to_explore.push(item.path);
                }
                _ => {
                    // Unknown type, skip
                }
            }
        }
        
        // For simplicity, we'll fetch one level at a time
        // In a more sophisticated implementation, we could parallelize this
        has_more = false; // We'll handle folders separately
        
        // Recursively fetch contents of each folder
        for folder in folders_to_explore {
            let folder_files = fetch_folder_contents(client, &repo_type, repo, &folder).await?;
            all_files.extend(folder_files);
        }
        
        page_count += 1;
    }
    
    // Sort files by path for better organization
    all_files.sort_by(|a, b| a.rfilename.cmp(&b.rfilename));
    
    println!("Found {} files, {} with LFS", all_files.len(), all_files.iter().filter(|f| f.lfs.is_some()).count());
    
    Ok(all_files)
}

// Helper function to recursively fetch folder contents
async fn fetch_folder_contents(
    client: &Client,
    repo_type: &str,
    repo: &str,
    folder_path: &str,
) -> Result<Vec<RepoFile>, Box<dyn std::error::Error + Send + Sync>> {
    let mut files = Vec::new();
    
    let url = format!(
        "https://huggingface.co/api/{}/{}/tree/main/{}",
        repo_type, repo, folder_path
    );
    
    let resp = client
        .get(&url)
        .header("User-Agent", "huggingface-downloader")
        .send()
        .await?;
        
    if !resp.status().is_success() {
        // Folder might be empty or inaccessible, skip it
        return Ok(files);
    }
    
    let json_resp = resp.json::<Vec<ApiFileInfo>>().await?;
    
    for item in json_resp {
        match item.file_type.as_str() {
            "file" => {
                files.push(RepoFile {
                    rfilename: item.path.clone(),
                    lfs: item.lfs,
                    size: item.size,
                });
            }
            "directory" => {
                // Recursively fetch subfolder contents
                let subfolder_files = Box::pin(fetch_folder_contents(
                    client,
                    repo_type,
                    repo,
                    &item.path,
                )).await?;
                files.extend(subfolder_files);
            }
            _ => {}
        }
    }
    
    Ok(files)
}

// Helper function to format bytes in human-readable format
fn format_bytes(bytes: i64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB", "TB", "PB"];
    let mut size = bytes as f64;
    let mut unit_index = 0;
    
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }
    
    if unit_index == 0 {
        format!("{} {}", size as i64, UNITS[unit_index])
    } else {
        format!("{:.2} {}", size, UNITS[unit_index])
    }
}

pub async fn fetch_sha256_hash(client: &Client, url: &str) -> Option<String> {
    let hash_url = format!("{}.sha256", url);

    match client.get(&hash_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            if let Ok(text) = resp.text().await {
                Some(text.split_whitespace().next()?.to_string())
            } else {
                None
            }
        }
        _ => None,
    }
}

pub async fn get_sha256_from_etag(client: &Client, url: &str) -> Option<String> {
    if let Ok(resp) = client.head(url).send().await {
        if let Some(etag) = resp.headers().get("x-linked-etag")
            .or_else(|| resp.headers().get("etag")) 
        {
            let tag = etag.to_str().ok()?;
            // Remove quotes and sha256: prefix if present
            let cleaned = tag.trim_matches('"');
            if cleaned.starts_with("sha256:") {
                Some(cleaned.trim_start_matches("sha256:").to_string())
            } else {
                Some(cleaned.to_string())
            }
        } else {
            None
        }
    } else {
        None
    }
}

pub async fn compute_sha256<P: AsRef<Path>>(path: P) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let file = File::open(path).await?;
    let mut reader = BufReader::with_capacity(10 * 1024 * 1024, file); // 1MB buffer for performance
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 1024 * 1024]; // 1MB chunks

    loop {
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            reader.read(&mut buffer)
        ).await {
            Ok(Ok(n)) => {
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(Err(e)) => {
                return Err(Box::new(e));
            }
            Err(_) => {
                return Err("Hash computation timed out".into());
            }
        }
    }

    let result = hasher.finalize();
    // Use explicit hex encoding like Go's hex.EncodeToString
    let hex_string = result.iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    Ok(hex_string)
}

pub async fn compute_sha1<P: AsRef<Path>>(path: P) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    let file = File::open(path).await?;
    let mut reader = BufReader::with_capacity(10 * 1024 * 1024, file); // 1MB buffer for performance
    let mut hasher = Sha1::new();
    let mut buffer = vec![0u8; 1024 * 1024]; // 1MB chunks

    loop {
        match tokio::time::timeout(
            std::time::Duration::from_secs(30),
            reader.read(&mut buffer)
        ).await {
            Ok(Ok(n)) => {
                if n == 0 {
                    break;
                }
                hasher.update(&buffer[..n]);
            }
            Ok(Err(e)) => {
                return Err(Box::new(e));
            }
            Err(_) => {
                return Err("Hash computation timed out".into());
            }
        }
    }

    let result = hasher.finalize();
    // Use explicit hex encoding like Go's hex.EncodeToString
    let hex_string = result.iter()
        .map(|byte| format!("{:02x}", byte))
        .collect::<String>();
    Ok(hex_string)
}

pub async fn compute_hash_matching_expected<P: AsRef<Path>>(path: P, expected_hash: &str) -> Result<String, Box<dyn std::error::Error + Send + Sync>> {
    // Determine hash type based on length
    match expected_hash.len() {
        40 => compute_sha1(path).await,  // SHA-1
        64 => compute_sha256(path).await, // SHA-256
        _ => {
            // Try SHA-256 by default for unknown lengths
            compute_sha256(path).await
        }
    }
}

pub async fn verify_downloaded_files(
    target_dir: &str,
    files: &Vec<RepoFile>,
    sha_map: &HashMap<String, String>,
    repo: &str,
    repo_type: &str,
    client: &Client,
) -> Vec<FileVerificationResult> {
    let mut verification_results = Vec::new();
    
    println!("\n🔍 Verifying downloaded files...");
    
    for f in files {
        let local_path = format!("{}/{}", target_dir, f.rfilename);
        
        // Skip if file doesn't exist
        if !tokio::fs::try_exists(&local_path).await.unwrap_or(false) {
            continue;
        }
        
        // Get expected hash
        let mut expected_hash = None;
        
        // For LFS files, use the oid from metadata
        if let Some(lfs_info) = &f.lfs {
            expected_hash = Some(lfs_info.oid.clone());
        } else {
            // Try to get hash from our sha_map or fetch it
            let mut remote_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, f.rfilename);
            if repo_type == "datasets" {
                remote_url = format!("https://huggingface.co/datasets/{}/resolve/main/{}", repo, f.rfilename);
            } else if repo_type == "spaces" {
                remote_url = format!("https://huggingface.co/spaces/{}/resolve/main/{}", repo, f.rfilename);
            }
            
            if let Some(hash) = sha_map.get(&remote_url) {
                expected_hash = Some(hash.clone());
            } else {
                // Try fetching from .sha256 file or etag
                if let Some(hash) = fetch_sha256_hash(&client, &remote_url).await {
                    expected_hash = Some(hash);
                } else if let Some(hash) = get_sha256_from_etag(&client, &remote_url).await {
                    expected_hash = Some(hash);
                }
            }
        }
        
        // If we have an expected hash, verify it
        if let Some(expected) = expected_hash {
            print!("Verifying {} ... ", f.rfilename);
            match compute_hash_matching_expected(&local_path, &expected).await {
                Ok(actual_hash) => {
                    let passed = actual_hash == expected;
                    if passed {
                        println!("✅ PASSED");
                    } else {
                        println!("❌ FAILED");
                    }
                    
                    verification_results.push(FileVerificationResult {
                        filename: f.rfilename.clone(),
                        expected_hash: expected,
                        actual_hash,
                        passed,
                    });
                }
                Err(e) => {
                    println!("❌ Error computing hash: {}", e);
                }
            }
        }
    }
    
    verification_results
}

pub async fn download_repo_files(
    context: Arc<AppContext>,
    repo_type: String,
    client: reqwest::Client,
    maxtry: u32,
    repo: &str,
    files: Vec<RepoFile>,
    mut multi: Arc<MultiProgress>,
    resumable: bool,
    skip_sha: bool
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let target_dir = repo.replace("/", "_");
    
    let mut max_parallel = 4;
    let mut max_chunk = 4;
    let mut max_size = None;
    if let Some(conf) = &context.config {
        max_parallel = conf.max_parallel;
        max_chunk = conf.max_chunk;
        max_size = conf.max_size;
    }
    
    let mut invalid_files: Vec<String> = vec![];
    let mut sha_map: HashMap<String, String> = HashMap::new();
    
    let cancel_notify = Arc::new(tokio::sync::Notify::new());
    
    // Collect SHA256 hashes for all files
    println!("📋 Collecting file metadata...");
    for f in &files {
        let mut remote_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, f.rfilename);
        if repo_type == "datasets" {
            remote_url = format!("https://huggingface.co/datasets/{}/resolve/main/{}", repo, f.rfilename);
        } else if repo_type == "spaces" {
            remote_url = format!("https://huggingface.co/spaces/{}/resolve/main/{}", repo, f.rfilename);
        }
        
        // For LFS files, store the hash from metadata
        if let Some(lfs_info) = &f.lfs {
            sha_map.insert(remote_url.clone(), lfs_info.oid.clone());
        }
    }
    
    // Download files
    for f in &files {
        let mut remote_url = format!("https://huggingface.co/{}/resolve/main/{}", repo, f.rfilename);
        if repo_type.to_string() == "datasets".to_string() {
            remote_url = format!("https://huggingface.co/datasets/{}/resolve/main/{}", repo, f.rfilename);
        }
        if repo_type.to_string() == "spaces".to_string() {
            remote_url = format!("https://huggingface.co/spaces/{}/resolve/main/{}", repo, f.rfilename);
        }
        let local_path = format!("{}/{}", target_dir, f.rfilename);
        
        if tokio::fs::try_exists(&local_path).await? {
            // For existing files, only verify LFS files if not skipping SHA
            if !skip_sha && f.lfs.is_some() {
                print!("Verifying existing LFS file {} ... ", &local_path);
                if let Some(lfs_info) = &f.lfs {
                    // Skip verification for very large files (> 5GB)
                    //if lfs_info.size > 5 * 1024 * 1024 * 1024 {
                    //    println!("skipped (file too large)");
                    //    continue;
                    //}
                    
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(300), // 5 minute timeout
                        compute_sha256(&local_path)
                    ).await {
                        Ok(Ok(actual_hash)) => {
                            if actual_hash == lfs_info.oid {
                                println!("✅ Hash matched");
                                continue;
                            } else {
                                println!("❌ Hash mismatch, re-downloading");
                                // Delete and re-download
                                fs::remove_file(&local_path).await?;
                            }
                        }
                        Ok(Err(e)) => {
                            println!("❌ Error computing hash: {}, re-downloading", e);
                            fs::remove_file(&local_path).await?;
                        }
                        Err(_) => {
                            println!("timeout, skipping");
                            continue;
                        }
                    }
                }
            } else {
                println!("✅ Skipped (exists): {}", &local_path);
                continue;
            }
        }
    
        if let Some(parent) = std::path::Path::new(&local_path).parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        
        {
            let run = *taskwait::ISRUNNING.lock().unwrap();
            if run == false {
                return Ok(());
            }
        }
        
        {
            let context2 = context.clone();
            let client2 = client.clone();
            let remote_url2 = remote_url.clone();
            let local_path2 = local_path.clone();
            let multi2 = multi.clone();
            
            let ct = context.taskwait.clone();
            let tw = &mut *ct.lock().unwrap();
            
            if let Some(c) = tw {
                let permit: OwnedSemaphorePermit = c.acquire_owned().await?;

                let handle = smart_dl::smart_download(Some(permit), context2.clone(), &client2, &remote_url2, &local_path2, maxtry, max_parallel as usize, max_chunk as usize, max_size.clone(), cancel_notify.clone(), multi.clone(), resumable).await;
                taskwait::add_task_handle(handle);
            } else {
                taskwait::wait_available_thread(max_parallel as i32);

                let handle = smart_dl::smart_download(None, context2.clone(), &client2, &remote_url2, &local_path2, maxtry, max_parallel as usize, max_chunk as usize, max_size.clone(), cancel_notify.clone(), multi.clone(), resumable).await;
                taskwait::add_task_handle(handle);
            }
        }
    }
    
    // Wait for all downloads to complete
    taskwait::wait_all_tasks().await;
    
    // Only verify LFS files if skip_sha is false
    if !skip_sha {
        println!("\n🔍 Verifying downloaded LFS files...");
        let mut verification_failed = false;
        let mut failed_files = Vec::new();
        
        for f in &files {
            // Only verify LFS files
            if let Some(lfs_info) = &f.lfs {
                let local_path = format!("{}/{}", target_dir, f.rfilename);
                
                if tokio::fs::try_exists(&local_path).await? {
                    // Skip verification for very large files
                    if lfs_info.size > 5 * 1024 * 1024 * 1024 {
                        continue;
                    }
                    
                    print!("Verifying {} ... ", f.rfilename);
                    
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(300), // 5 minute timeout
                        compute_sha256(&local_path)
                    ).await {
                        Ok(Ok(actual_hash)) => {
                            if actual_hash == lfs_info.oid {
                                println!("✅ PASSED");
                            } else {
                                println!("❌ FAILED");
                                verification_failed = true;
                                failed_files.push(FileVerificationResult {
                                    filename: f.rfilename.clone(),
                                    expected_hash: lfs_info.oid.clone(),
                                    actual_hash,
                                    passed: false,
                                });
                                
                                // Delete the corrupted file
                                if let Err(e) = fs::remove_file(&local_path).await {
                                    println!("   Failed to delete corrupted file: {}", e);
                                }
                            }
                        }
                        Ok(Err(e)) => {
                            println!("❌ Error computing hash: {}", e);
                        }
                        Err(_) => {
                            println!("timeout");
                        }
                    }
                }
            }
        }
        
        if verification_failed {
            println!("\n⚠️  Hash verification failed for {} LFS file(s):", failed_files.len());
            println!("┌─────────────────────────────────────────");
            for result in &failed_files {
                println!("❌ {}", result.filename);
                println!("   Expected: {}", result.expected_hash);
                println!("   Actual:   {}", result.actual_hash);
                println!("   🗑️  File deleted, please re-run to download");
            }
            println!("└─────────────────────────────────────────");
            println!("\n🔄 Please run the program again to re-download the corrupted files.");
            
            return Err("Some LFS files failed hash verification. Please run the program again.".into());
        } else {
            let lfs_count = files.iter().filter(|f| f.lfs.is_some()).count();
            if lfs_count > 0 {
                println!("\n✅ All {} LFS files passed hash verification!", lfs_count);
            } else {
                println!("\n✅ No LFS files to verify");
            }
        }
    } else {
        println!("\n⚠️  Hash verification skipped (--skip-sha flag was used)");
    }
    
    Ok(())
}