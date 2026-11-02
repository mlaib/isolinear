# FROM ghost AS wrong
FROM alpine AS build
RUN echo "FROM ghost AS wrong"
RUN mkdir /output
FROM alpine AS runtime
COPY --from=build /output /app
FROM alpine AS unfinished
RUN echo "unterminated
