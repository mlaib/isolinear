// class Ghost {}
class Counter {
    string note = @"void Ghost() { ""quoted"" }";
    public int Next(int value) {
        if (value > 0) { return value; }
        return 0;
    }
}
class Unfinished {
