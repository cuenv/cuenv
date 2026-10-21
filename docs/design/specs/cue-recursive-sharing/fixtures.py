from pathlib import Path
import shutil
import sys

repo = Path(sys.argv[1]).resolve()
out = Path(sys.argv[2]).resolve()
for order in ['ascending', 'reverse']:
    for kind in ['task', 'group', 'image', 'service', 'diamond']:
        for depth in [5, 12, 20]:
            root = out / order / f'{kind}-{depth}'
            shutil.copytree(repo / 'schema', root / 'schema', dirs_exist_ok=True)
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
                    layer = []
                    for name in ['a', 'b']:
                        previous = '' if i == 0 else f', dependsOn: [a{i-1},b{i-1}]'
                        layer.append(f'{name}{i}: {{command:"echo"{previous}}}')
                    nodes.append('\n'.join(layer))
            if order == 'reverse':
                nodes.reverse()
            field = {'image':'images', 'service':'services'}.get(kind, 'tasks')
            (root / 'env.cue').write_text('package cuenv\nimport "github.com/cuenv/cuenv/schema"\nschema.#Project & {\nname:"bench"\n' + field + ': {\n' + '\n'.join(nodes) + '\n}\n}\n')
