import os

def f(path):
    with open(path, 'r', encoding='utf-8') as f:
        c = f.read()
    # just find the conflict markers
    return c
