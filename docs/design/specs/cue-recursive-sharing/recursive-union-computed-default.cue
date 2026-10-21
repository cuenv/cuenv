package cuenv

#Node: #Task | #Group | #Sequence
#Task: {
	type?:    _|_
	_prefix:  string | *""
	_self:    string | *""
	_name:    string | *(_prefix + _self)
	command!: string
	dependsOn?: [...(#Node | #Image)]
}
#Group: {
	type: "group"
}
#Image: {type: "image", context!: string, dependsOn?: [...(#Node | #Image)]}
#Sequence: [...#Node]
tasks: [N=string]: ((#Task | #Group) & {_prefix: "", _self: N}) | #Sequence
tasks: {
	n0: {command: "echo"}
	n1: {command: "echo", dependsOn: [n0]}
	n2: {command: "echo", dependsOn: [n1]}
}
