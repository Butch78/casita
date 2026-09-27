"""Restore and retain this Mac experiment's exact Nix fixture closures."""
import glob
import json
from pathlib import Path
import subprocess

paths=glob.glob('/nix/store/*nix-*/bin/nix-store')
assert paths, 'nix-store is required to retain Nix-backed fixtures'
nix=next((p for p in paths if 'nix-2.35.2/' in p),paths[-1])
report=json.loads(Path('results/workloads-stable-cache.json').read_text())
roots=Path('results/fixture-roots').resolve()
roots.mkdir(exist_ok=True)
receipts={}
for tool,info in report['workload_fixture']['tools'].items():
    assert info['source'].startswith('/nix/store/')
    store='/'.join(info['source'].split('/')[:4])
    root=roots/tool
    subprocess.run([nix,'--realise',store,'--add-root',str(root),'--indirect'],check=True)
    receipts[tool]={'root':str(root),'store':store}
Path('results/fixture-gc-roots.json').write_text(json.dumps(receipts,indent=2)+'\n')
