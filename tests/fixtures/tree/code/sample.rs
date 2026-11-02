/* fn ghost() {} */
const NOTE: &str = r##"fn ghost() { " }"##;
mod tools {
    struct Counter { value: usize }
    impl Display for Counter {
        fn fmt<'a>(&'a self) { let ch = '{'; }
    }
}
fn unfinished() {
