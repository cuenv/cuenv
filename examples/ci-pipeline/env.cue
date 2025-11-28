package cuenv

#Cuenv: {
    ci?: {
        pipelines: [...{
            name: string
            tasks: [...string]
        }]
    }
    tasks: [string]: {
        command: string
        args?: [...string]
        inputs?: [...string]
    }
}

#Cuenv

ci: pipelines: [
    {
        name: "default"
        tasks: ["test"]
    }
]

tasks: {
    test: {
        command: "echo"
        args: ["Running test task"]
        inputs: ["env.cue"]
    }
}
