/* int ghost(void) {} */
const char *note = "int ghost(void) { }";
struct Packet {
    struct Header { int length; } header;
};
int sum(int left, int right) {
    if (left) { return left + right; }
    return right;
}
int unfinished(void) {
