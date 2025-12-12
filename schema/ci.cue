package schema

#PipelineCondition: close({
	pullRequest?: bool
	branch?: string | [...string]
	tag?: string | [...string]
	defaultBranch?: bool
	scheduled?:     bool
	manual?:        bool
})

#Pipeline: close({
	name:  string
	when?: #PipelineCondition
	tasks: [...string]
})

#CI: close({
	pipelines: [...#Pipeline]
})
