#!/bin/sh
# ghost() {}
note='ghost() { echo wrong; }'
function outer {
    inner() { printf '%s' ready; }
    inner
}
unfinished() {
