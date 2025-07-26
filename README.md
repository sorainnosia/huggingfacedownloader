# HuggingFaceDownloader

A tiny Huggingface repository downloader.

Download files from HuggingFace repository easily, parallel 4 files download. If url supports range, the file will be downloaded in chunks.

## Example
```
huggingfacedownloader -j moonshotai/Kimi-K2-Instruct -m 4
```
