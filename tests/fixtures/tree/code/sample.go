package example
// func ghost() {}
var note = `func ghost() {}`
type Counter struct { value int }
func (c *Counter) Add(value int) int {
    type Delta struct { amount int }
    if value > 0 { c.value += value }
    return c.value
}
func unfinished() {
