import { $ } from "bun";

console.log(`bun received: ${JSON.stringify(Bun.argv.slice(2))}`);
await $`echo bun shell works`;
