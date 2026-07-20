FROM sysg-usecase-base

COPY tests/docker/usecase/logs-purge-clears/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/logs-purge-clears/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
