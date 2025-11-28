package schema

#PipelineCondition: {
	pullRequest?:   bool
	branch?:        string | [...string]
	tag?:           string | [...string]
	defaultBranch?: bool
	scheduled?:     bool
	manual?:        bool
}

#Pipeline: {
	name: string
	when?: #PipelineCondition
	tasks: [...string]
}

#CI: {
	pipelines: [...#Pipeline]
}
