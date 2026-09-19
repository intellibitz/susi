import urllib.request
import json
import urllib.error

def fetch_model_info(repo):
    try:
        url = f"https://huggingface.co/api/models/{repo}/tree/main"
        req = urllib.request.Request(url)
        with urllib.request.urlopen(req) as response:
            files = json.loads(response.read().decode())
            gguf_files = [f for f in files if f['path'].endswith('.gguf')]
            for f in gguf_files:
                size = f.get('size', 0)
                print(f"File: {f['path']}, Size: {size}")
    except Exception as e:
        print("Error fetching repo:", repo, e)
fetch_model_info("Qwen/Qwen2.5-1.5B-Instruct-GGUF")
fetch_model_info("unsloth/DeepSeek-R1-Distill-Llama-70B-GGUF")
