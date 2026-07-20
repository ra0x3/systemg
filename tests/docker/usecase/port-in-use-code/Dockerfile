FROM sysg-usecase-base

COPY tests/docker/usecase/port-in-use-code/stack.yaml /usecase/stack.yaml
COPY tests/docker/usecase/port-in-use-code/run.sh /usecase/run.sh
RUN chmod +x /usecase/run.sh

ENTRYPOINT ["/usecase/run.sh"]
