// void ghost() {}
const char *note = R"tag(void ghost() { " })tag";
namespace tools {
    class Counter {
    public:
        int next() const { return 1; }
    };
}
void unfinished() {
