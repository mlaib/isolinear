// function Ghost() {}
const note = "function Ghost() {}";
function View() {
    const label = () => { return <span>Ready</span>; };
    return <div>{label()}</div>;
}
function unfinished() {
