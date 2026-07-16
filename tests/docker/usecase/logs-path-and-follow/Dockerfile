FROM sysg-usecase-base

COPY tests/docker/usecase/logs-path-and-follow/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/logs-path-and-follow/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
