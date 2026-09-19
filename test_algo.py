import urllib.request
import json
url = "https://huggingface.co/api/models?search=Instruct-GGUF&sort=downloads&limit=10"
req = urllib.request.Request(url)
try:
    with urllib.request.urlopen(req) as response:
        data = json.loads(response.read().decode())
        for m in data:
            print(m['id'])
except Exception as e:
    print(e)
