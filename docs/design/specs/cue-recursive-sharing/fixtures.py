from pathlib import Path
import io
import shutil
import subprocess
import sys
import tarfile

repo = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
old = out / 'pre515'
old.mkdir(parents=True, exist_ok=True)
archive = subprocess.check_output(['git', 'archive', '092d0c9665079cd7945ad3205b10952d91b8a72b', 'schema'], cwd=repo)
tarfile.open(fileobj=io.BytesIO(archive)).extractall(old, filter='data')
for schema in ['typed', 'untyped']:
    source = old / 'schema' if schema == 'typed' else repo / 'schema'
    for kind in ['task', 'group', 'image', 'service', 'diamond']:
        for depth in [5, 12, 20]:
            root = out / schema / f'{kind}-{depth}'
            shutil.copytree(source, root / 'schema', dirs_exist_ok=True)
            (root / 'cue.mod').mkdir(exist_ok=True)
            (root / 'cue.mod/module.cue').write_text('module: "github.com/cuenv/cuenv"\nlanguage: version: "v0.14.1"\n')
            nodes = []
            for i in range(depth + 1):
                deps = '' if i == 0 else f', dependsOn: [n{i-1}]'
                if kind == 'task': nodes.append(f'n{i}: {{command:"echo"{deps}}}')
                if kind == 'group': nodes.append(f'n{i}: {{type:"group", unit: {{command:"echo"}}{deps}}}')
                if kind == 'image': nodes.append(f'n{i}: {{context:"."{deps}}}')
                if kind == 'service': nodes.append(f'n{i}: {{entrypoint: schema.#Command & {{command:"echo"}}{deps}}}')
                if kind == 'diamond':
                    for name in ['a', 'b']:
                        previous = '' if i == 0 else f', dependsOn: [a{i-1},b{i-1}]'
                        nodes.append(f'{name}{i}: {{command:"echo"{previous}}}')
            field = {'image':'images', 'service':'services'}.get(kind, 'tasks')
            (root / 'env.cue').write_text('package cuenv\nimport "github.com/cuenv/cuenv/schema"\nschema.#Project & {\nname:"bench"\n' + field + ': {\n' + '\n'.join(nodes) + '\n}\n}\n')
