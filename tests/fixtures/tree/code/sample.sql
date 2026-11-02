-- CREATE TABLE ghost (id int);
SELECT 'CREATE TABLE ghost (id int);';
CREATE TABLE app.items (id INTEGER, label TEXT);
create or replace VIEW app.labels AS SELECT label FROM app.items;
CREATE FUNCTION next_value() RETURNS INT AS $$
    CREATE TABLE ghost (id int);
$$ LANGUAGE SQL;
CREATE TABLE unfinished (
