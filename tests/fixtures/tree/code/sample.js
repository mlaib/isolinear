// function ghost() {}
const note = `class Ghost { method() {} }`;
class Counter {
    next(value) { return value + 1; }
}
const worker = (value) => {
    function nested() { return value; }
    return nested();
};
let other = function named() { return 1; };
function unfinished() {
