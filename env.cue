package cuenv

import "github.com/cuenv/cuenv/schema"

schema.#Cuenv

env: {
  NAME: "david"
}

tasks: {
  clippy: {
    shell: {command: "bash", flag: "-c"}
    command: "devenv shell -- cargo clippy -- -D warnings"
  }
}

