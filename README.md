# HuggingFaceDownloader

A tiny Huggingface repository downloader.

Download files from HuggingFace repository easily, support parallel files download by default `1` file at a time, to override use `-m 2` for 2 files at a time.
If url supports range, the file will be downloaded parallel in chunks by default `7` chunks per file, to override use `-c 4` for 4 chunk.

Downloader support download of HuggingFace `datasets` and `spaces` other than the default `models`, to override use `-t datasets` for datasets and `-t spaces` for spaces.

Downloader support resume by default (if URL support it), and download will be resume on top of completed chunks, to make it better if you are intending to resume use `-p 100MB` option to slice file based on a fix resumable size, you need to use this argument again during re-run.

## Example
Download `models` `moonshotai/Kimi-K2-Instruct` with `4` files parallel at a time (default 7 chunks per file)
```
huggingfacedownloader -j moonshotai/Kimi-K2-Instruct -m 4
```

Download `datasets` `facebook/flores` with `2` files parallel at a time with 1 chunk per file
```
huggingfacedownloader -j facebook/flores -m 2 -t datasets -c 1
```

Download `models` `private/repository` with `1` files parallel at a time with each file `4` chunks and `-k api_key`
```
huggingfacedownloader -j moonshotai/Kimi-K2-Instruct -m 1 -c 4 -k s3cr3tap1k3y...
```

Download without enabling resumable and if drops, file redownload again
```
huggingfacedownloader -j moonshotai/Kimi-K2-Instruct -n
```

**Download by slicing file with each slice `100MB` in case of connection drops, resume will be faster**
```
huggingfacedownloader -j moonshotai/Kimi-K2-Instruct -p 100MB
```
