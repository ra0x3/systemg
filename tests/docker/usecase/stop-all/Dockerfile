FROM sysg-usecase-base

COPY tests/docker/usecase/stop-all/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/stop-all/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
