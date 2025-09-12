package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

env: {
  NAME: "david"
}

tasks: {
  clippy: {
    shell: {command: "bash", flag: "-c"}
    command: "nix develop --command cargo clippy -- -D warnings"
  }
}

