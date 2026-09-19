print("Checking HF API...")
import urllib.request
import json

url = "https://huggingface.co/api/models?search=Qwen2.5&sort=downloads&limit=5&filter=gguf"
req = urllib.request.Request(url)
with urllib.request.urlopen(req) as response:
    data = json.loads(response.read().decode())
    for m in data:
        print(m['id'])
