# def ghost(): pass
note = """class Ghost:
    def ghost(): pass
"""
@decorate
class Counter:
    @cached
    async def next(self):
        def nested():
            return 1
        return nested()

def unfinished(
