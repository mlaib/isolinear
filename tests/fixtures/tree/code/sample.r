# ghost <- function() {}
note <- "ghost <- function() {}"
outer <- function(values) {
    inner <- function(value) { value + 1 }
    lapply(values, inner)
}
unfinished <- function() {
