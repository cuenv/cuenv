//! Tests for CUE evaluation and JSON parsing

#[cfg(test)]
mod tests {
    use crate::environment::CueEvaluation;
    use cuengine::CueEvaluator;
    use std::path::Path;
    
    #[test]
    fn test_parse_task_basic_example() {
        // Get the project root (where Cargo.toml is)
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let project_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
        let example_path = project_root.join("examples/task-basic");
        
        // Skip test if example doesn't exist
        if !example_path.exists() {
            println!("Skipping test - example path doesn't exist: {:?}", example_path);
            return;
        }
        
        // Evaluate the actual example CUE file
        let evaluator = CueEvaluator::builder().build().unwrap();
        let json = evaluator
            .evaluate(&example_path, "examples")
            .unwrap();
        
        println!("Raw JSON from CUE evaluation:");
        println!("{}", json);
        
        // Parse the JSON to see what structure we get
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        println!("\nParsed JSON structure:");
        println!("{:#?}", parsed);
        
        // Check what fields are present at the root
        if let serde_json::Value::Object(map) = &parsed {
            println!("\nRoot level fields:");
            for key in map.keys() {
                println!("  - {}", key);
            }
        }
        
        // Try to parse as CueEvaluation
        let evaluation = CueEvaluation::from_json(&json);
        match evaluation {
            Ok(eval) => {
                println!("\nSuccessfully parsed as CueEvaluation");
                let env = eval.get_environment();
                println!("Environment variables: {:?}", env.vars);
                println!("Tasks: {:?}", eval.tasks.list_tasks());
                
                // Verify we got the expected values
                assert_eq!(env.get("NAME"), Some("Jack O'Neill"));
                assert!(eval.tasks.contains("interpolate"));
                assert!(eval.tasks.contains("propagate"));
                assert!(eval.tasks.contains("greetAll"));
                assert!(eval.tasks.contains("greetIndividual"));
            }
            Err(e) => {
                println!("\nFailed to parse as CueEvaluation: {}", e);
                panic!("Failed to parse CUE evaluation");
            }
        }
    }
    
    #[test]
    fn test_parse_env_basic_example() {
        // Get the project root
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        let project_root = Path::new(manifest_dir).parent().unwrap().parent().unwrap();
        let example_path = project_root.join("examples/env-basic");
        
        // Skip test if example doesn't exist
        if !example_path.exists() {
            println!("Skipping test - example path doesn't exist: {:?}", example_path);
            return;
        }
        
        // Test with env-basic example
        let evaluator = CueEvaluator::builder().build().unwrap();
        let json = evaluator
            .evaluate(&example_path, "examples")
            .unwrap();
        
        println!("Raw JSON from env-basic:");
        println!("{}", json);
        
        // Parse to inspect structure
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        
        // Check for env field
        if let Some(env) = parsed.get("env") {
            println!("\nFound 'env' field:");
            println!("{:#?}", env);
        } else {
            println!("\nNo 'env' field found at root level");
        }
    }
}