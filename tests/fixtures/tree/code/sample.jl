# function ghost() end
note = "function ghost() end"
module Tools
    struct Counter
        value::Int
    end
    function next(value)
        function inner()
            value + 1
        end
        inner()
    end
end
function unfinished()
