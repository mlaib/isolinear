// class Ghost {}
class Counter {
    String note = "void ghost() {}";
    public int next(int value) {
        while (value < 0) { value++; }
        return value;
    }
}
class Unfinished {
