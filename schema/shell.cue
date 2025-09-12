package schema

#Shell: {
	command: string // Shell executable name (e.g., "bash", "fish", "zsh")
	flag:    string // Flag for command execution (e.g., "-c", "--command")
}

#Bash: #Shell & {
	command: "bash"
	flag:    "-c"
}

#Fish: #Shell & {
	command: "fish"
	flag:    "-c"
}

#Zsh: #Shell & {
	command: "zsh"
	flag:    "-c"
}

#Sh: #Shell & {
	command: "sh"
	flag:    "-c"
}

#Cmd: #Shell & {
	command: "cmd"
	flag:    "/C"
}

#PowerShell: #Shell & {
	command: "powershell"
	flag:    "-Command"
}
