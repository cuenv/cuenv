def main [...names: string] {
	$names | each {|name| $"nu says hello to ($name)" } | str join "\n" | print
}
