-- function ghost() end
local note = [=[function ghost() end]=]
local function outer()
    function inner()
        return 1
    end
    return inner()
end
function unfinished()
