use stacksafe::{StackSafe, stacksafe};

#[stacksafe]
fn count(depth: usize) -> usize {
    if depth == 0 { 0 } else { 1 + count(depth - 1) }
}

struct Node {
    _next: Option<Box<StackSafe<Node>>>,
}

#[test]
fn grows_the_stack_for_recursion_and_recursive_destruction() {
    let result = std::thread::Builder::new()
        .stack_size(64 * 1024)
        .spawn(|| {
            assert_eq!(count(100_000), 100_000);
            let mut next = None;
            for _ in 0..100_000 {
                next = Some(Box::new(StackSafe::new(Node { _next: next })));
            }
            drop(next);
        })
        .and_then(|thread| {
            thread
                .join()
                .map_err(|_| std::io::Error::other("stack-safety test thread panicked"))
        });
    assert!(result.is_ok(), "{result:?}");
}

#[test]
fn keeps_the_expected_default_stack_sizes() {
    assert_eq!(stacksafe::get_minimum_stack_size(), 128 * 1024);
    assert_eq!(stacksafe::get_stack_allocation_size(), 2 * 1024 * 1024);
}
