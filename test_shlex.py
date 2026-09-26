import shlex
s = "sh -c 'echo '\\''susi local models work definitively'\\'' > final_proof.txt'"
print(shlex.split(s))
