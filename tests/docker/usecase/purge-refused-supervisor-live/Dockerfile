FROM sysg-usecase-base

COPY tests/docker/usecase/purge-refused-supervisor-live/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/purge-refused-supervisor-live/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
