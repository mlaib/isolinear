// interface Ghost {}
const note = "function ghost() {}";
interface Item { value: number; }
class Counter {
    next(value: number): number { return value + 1; }
}
const worker = async (value: number): Promise<number> => {
    function nested() { return value; }
    return nested();
};
function unfinished() {
